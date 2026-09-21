use super::*;
use std::path::PathBuf;
use vault::frontmatter::Frontmatter;

fn canon() -> CanonicalSet {
    let file: vault::canonical::CanonicalTagsFile = serde_yaml::from_str(
        "max-per-note: 8\nmax-canonical: 300\ntags:\n  tech:\n    - rust\n    - cli\n  ai:\n    - ai\n",
    )
    .expect("canonical fixture");
    file.canonical_set()
}

fn mapping() -> TagMapping {
    serde_yaml::from_str("ai-agents: ai\nrustlang: rust\ndocumentation: null\nsession-management: null\n")
        .expect("mapping fixture")
}

fn staged(trace: &str, tags: &[&str]) -> StagedCandidates {
    StagedCandidates {
        trace: trace.to_string(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
    }
}

fn note(path: &str, trace: Option<&str>, source: Option<&str>) -> Note {
    Note {
        path: PathBuf::from(path),
        frontmatter: Frontmatter {
            trace: trace.map(str::to_string),
            source: source.map(str::to_string),
            ..Default::default()
        },
        body: String::new(),
        raw: String::new(),
    }
}

// ---------------------------------------------------------------------------
// aggregate: pure, hand-built inputs, no filesystem.
// ---------------------------------------------------------------------------

#[test]
fn aggregate_reproduces_a_known_frequency_and_honors_the_threshold() {
    let staged = [
        staged("t1", &["ci-cd", "helm"]),
        staged("t2", &["ci-cd"]),
        staged("t3", &["ci-cd"]),
    ];
    let out = aggregate(&[], &staged, &HashMap::new(), &canon(), &mapping(), 3);
    assert_eq!(out.len(), 1, "only ci-cd clears a threshold of 3: {out:?}");
    assert_eq!(out[0].tag, "ci-cd");
    assert_eq!(out[0].frequency, 3);
    assert_eq!(out[0].source, ProposalSource::Staged);
}

#[test]
fn a_trace_listing_one_tag_twice_contributes_one() {
    // `read_staged_candidates` dedupes within a trace, and `aggregate` counts
    // distinct source KEYS, so a duplicate cannot inflate either way.
    let staged = [staged("t1", &["ci-cd", "ci-cd"]), staged("t2", &["ci-cd"])];
    let out = aggregate(&[], &staged, &HashMap::new(), &canon(), &mapping(), 2);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].frequency, 2, "one trace votes once: {:?}", out[0]);
}

/// The M1 regression canary, frozen from the live corpus: `ht-970437f1` and
/// `ht-ba9e3191` are both `succeeded` against
/// `notes/pi-coding-agent-free-course.md` and both carry `system-prompt`. Two
/// traces, ONE note, so the tag scores 2 and must not reach a threshold of 3.
#[test]
fn two_traces_resolving_to_one_note_contribute_one() {
    let staged = [
        staged("ht-970437f1", &["system-prompt"]),
        staged("ht-ba9e3191", &["system-prompt"]),
        staged("ht-other", &["system-prompt"]),
    ];
    let mut trace_to_note = HashMap::new();
    trace_to_note.insert(
        "ht-970437f1".to_string(),
        "notes/pi-coding-agent-free-course.md".to_string(),
    );
    trace_to_note.insert(
        "ht-ba9e3191".to_string(),
        "notes/pi-coding-agent-free-course.md".to_string(),
    );
    trace_to_note.insert("ht-other".to_string(), "notes/elsewhere.md".to_string());

    let out = aggregate(&[], &staged, &trace_to_note, &canon(), &mapping(), 3);
    assert!(
        out.is_empty(),
        "system-prompt scores 2 from 3 traces and must not reach 3: {out:?}"
    );

    let out2 = aggregate(&[], &staged, &trace_to_note, &canon(), &mapping(), 2);
    assert_eq!(out2.len(), 1);
    assert_eq!(out2[0].frequency, 2, "two distinct notes, not three traces");
}

#[test]
fn an_unresolvable_trace_still_contributes_its_own_key() {
    let staged = [staged("t1", &["jsonl"]), staged("t2", &["jsonl"])];
    let out = aggregate(&[], &staged, &HashMap::new(), &canon(), &mapping(), 2);
    assert_eq!(out[0].frequency, 2);
    assert_eq!(out[0].sources, vec!["trace:t1", "trace:t2"]);
}

/// The two arms share one key space (vault-relative note paths), so a note
/// that carries the tag in frontmatter AND produced a staged trace with it
/// counts once - and the proposal is marked `both`.
#[test]
fn the_two_arms_share_a_key_space_and_do_not_double_count() {
    let note_candidates = [("notes/a.md".to_string(), vec!["jsonl".to_string()])];
    let staged = [staged("t1", &["jsonl"]), staged("t2", &["jsonl"])];
    let mut trace_to_note = HashMap::new();
    trace_to_note.insert("t1".to_string(), "notes/a.md".to_string());
    trace_to_note.insert("t2".to_string(), "notes/b.md".to_string());

    let out = aggregate(&note_candidates, &staged, &trace_to_note, &canon(), &mapping(), 2);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].frequency, 2, "notes/a.md is one source, not two: {:?}", out[0]);
    assert_eq!(out[0].sources, vec!["notes/a.md", "notes/b.md"]);
    assert_eq!(out[0].source, ProposalSource::Both);
}

#[test]
fn a_candidate_that_resolves_canonically_is_dropped() {
    // `rustlang` maps to `rust`, `ai-agents` maps to `ai`, `rust` is an exact
    // member, and `cli` segment-matches. None is a candidate.
    let staged = [
        staged("t1", &["rustlang", "ai-agents", "rust", "cli"]),
        staged("t2", &["rustlang", "ai-agents", "rust", "cli"]),
    ];
    let out = aggregate(&[], &staged, &HashMap::new(), &canon(), &mapping(), 2);
    assert!(out.is_empty(), "everything here resolves: {out:?}");
}

/// Phase 1's guard, enforced on the staged arm too: a `null` mapping is a
/// human reject and must never be re-proposed.
#[test]
fn a_human_rejected_candidate_is_dropped() {
    let staged = [
        staged("t1", &["documentation", "session-management", "jsonl"]),
        staged("t2", &["documentation", "session-management", "jsonl"]),
    ];
    let out = aggregate(&[], &staged, &HashMap::new(), &canon(), &mapping(), 2);
    assert_eq!(out.len(), 1, "only jsonl survives: {out:?}");
    assert_eq!(out[0].tag, "jsonl");
}

#[test]
fn sources_are_capped_at_five_while_frequency_counts_them_all() {
    let staged: Vec<StagedCandidates> = (0..9).map(|i| staged(&format!("t{i}"), &["jsonl"])).collect();
    let out = aggregate(&[], &staged, &HashMap::new(), &canon(), &mapping(), 3);
    assert_eq!(out[0].frequency, 9, "frequency is the full count");
    assert_eq!(out[0].sources.len(), MAX_SAMPLE_SOURCES, "provenance is a sample");
}

#[test]
fn ordering_is_frequency_descending_then_tag() {
    let staged = [
        staged("t1", &["aaa", "zzz", "mmm"]),
        staged("t2", &["aaa", "zzz", "mmm"]),
        staged("t3", &["zzz"]),
    ];
    let out = aggregate(&[], &staged, &HashMap::new(), &canon(), &mapping(), 2);
    let order: Vec<&str> = out.iter().map(|p| p.tag.as_str()).collect();
    assert_eq!(order, vec!["zzz", "aaa", "mmm"], "3 then 2,2 sorted by tag");
}

// ---------------------------------------------------------------------------
// read_staged_candidates: a tempfile tree, never the live staging root.
// ---------------------------------------------------------------------------

fn write_trace(root: &Path, trace: &str, body: &str) {
    let dir = root.join(trace);
    std::fs::create_dir_all(&dir).expect("trace dir");
    std::fs::write(dir.join("distilled.yml"), body).expect("write distilled");
}

#[test]
fn a_missing_staging_root_skips_the_staged_arm_without_failing() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let scan = read_staged_candidates(&dir.path().join("does-not-exist")).expect("must not fail");
    assert!(!scan.scanned, "a host with no staging tree is not an error");
    assert!(scan.candidates.is_empty());
}

#[test]
fn a_good_tree_yields_deduped_normalized_candidates_and_a_window() {
    let dir = tempfile::tempdir().expect("tmpdir");
    write_trace(
        dir.path(),
        "hv-1",
        "tags:\n  - CI-CD\n  - ci-cd\n  - helm\nmeta:\n  produced-at: 2026-08-05T00:00:00Z\n",
    );
    write_trace(
        dir.path(),
        "hv-2",
        "tags:\n  - jsonl\nmeta:\n  produced-at: 2026-09-21T00:00:00Z\n",
    );

    let scan = read_staged_candidates(dir.path()).expect("scan");
    assert!(scan.scanned);
    assert_eq!(scan.candidates.len(), 2);
    let one = scan.candidates.iter().find(|c| c.trace == "hv-1").expect("hv-1");
    assert_eq!(one.tags, vec!["ci-cd", "helm"], "normalized and deduped within a trace");
    assert_eq!(
        scan.window_label().as_deref(),
        Some("2026-08-05T00:00:00Z .. 2026-09-21T00:00:00Z")
    );
}

/// A trace directory with no `distilled.yml` is the ORDINARY case (45,637 of
/// 46,296 on the daemon host): counted, never logged, never an error.
#[test]
fn a_trace_without_distilled_is_counted_not_failed() {
    let dir = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(dir.path().join("hv-empty")).expect("bare trace dir");
    write_trace(dir.path(), "hv-1", "tags:\n  - jsonl\n");

    let scan = read_staged_candidates(dir.path()).expect("scan");
    assert_eq!(scan.without_distilled, 1);
    assert_eq!(scan.candidates.len(), 1);
    assert_eq!(scan.unreadable, 0);
}

#[test]
fn a_malformed_distilled_is_one_omission_not_a_failure() {
    let dir = tempfile::tempdir().expect("tmpdir");
    write_trace(dir.path(), "hv-bad", "tags: [unclosed\n");
    write_trace(dir.path(), "hv-ok", "tags:\n  - jsonl\n");

    let scan = read_staged_candidates(dir.path()).expect("a bad file must not abort the scan");
    assert_eq!(scan.unreadable, 1);
    assert_eq!(scan.candidates.len(), 1);
    assert_eq!(scan.candidates[0].trace, "hv-ok");
}

#[test]
fn a_distilled_with_no_tags_key_yields_an_empty_candidate_list() {
    let dir = tempfile::tempdir().expect("tmpdir");
    write_trace(dir.path(), "hv-1", "summary: nothing to see\n");
    let scan = read_staged_candidates(dir.path()).expect("scan");
    assert_eq!(scan.candidates.len(), 1);
    assert!(scan.candidates[0].tags.is_empty());
}

// ---------------------------------------------------------------------------
// resolve_trace_notes: tiers, against a tempfile receipts DB.
// ---------------------------------------------------------------------------

fn receipts_db(dir: &Path, rows: &[(&str, Option<&str>, &str)]) -> PathBuf {
    let path = dir.join("receipts.db");
    let conn = rusqlite::Connection::open(&path).expect("create db");
    conn.execute_batch(
        "CREATE TABLE receipts (
           trace_id TEXT NOT NULL PRIMARY KEY,
           note_path TEXT,
           raw_input TEXT NOT NULL
         );",
    )
    .expect("schema");
    for (trace, note_path, raw_input) in rows {
        conn.execute(
            "INSERT INTO receipts (trace_id, note_path, raw_input) VALUES (?1, ?2, ?3)",
            rusqlite::params![trace, note_path, raw_input],
        )
        .expect("insert");
    }
    path
}

#[test]
fn tier_one_the_vault_back_edge_wins() {
    let dir = tempfile::tempdir().expect("tmpdir");
    // The receipt points at a STALE location; the note's own `trace:` is
    // authoritative and must win.
    let db = receipts_db(dir.path(), &[("hv-1", Some("/vault/inbox/a.md"), "https://x")]);
    let notes = vec![note("notes/a.md", Some("hv-1"), Some("https://x"))];

    let out = resolve_trace_notes(&db, Path::new("/vault"), &notes).expect("resolve");
    assert_eq!(out.get("hv-1").map(String::as_str), Some("notes/a.md"));
}

#[test]
fn tier_two_uses_the_recorded_path_when_it_still_exists() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let db = receipts_db(dir.path(), &[("hv-1", Some("/vault/notes/a.md"), "https://x")]);
    // No note carries `trace: hv-1`, so tier 1 cannot fire.
    let notes = vec![note("notes/a.md", None, Some("https://x"))];

    let out = resolve_trace_notes(&db, Path::new("/vault"), &notes).expect("resolve");
    assert_eq!(out.get("hv-1").map(String::as_str), Some("notes/a.md"));
}

#[test]
fn tier_three_requires_the_source_to_match() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let db = receipts_db(dir.path(), &[("hv-1", Some("/vault/inbox/moved.md"), "https://x")]);

    // Same basename, DIFFERENT source: the basename now belongs to another
    // note, and tier 3 must refuse it.
    let wrong = vec![note("notes/moved.md", None, Some("https://different"))];
    let out = resolve_trace_notes(&db, Path::new("/vault"), &wrong).expect("resolve");
    assert!(out.is_empty(), "a source mismatch must not resolve: {out:?}");

    // Same basename, matching source: this is the reingested-note case tier 3
    // exists for.
    let right = vec![note("notes/moved.md", None, Some("https://x"))];
    let out = resolve_trace_notes(&db, Path::new("/vault"), &right).expect("resolve");
    assert_eq!(out.get("hv-1").map(String::as_str), Some("notes/moved.md"));
}

#[test]
fn tier_four_leaves_the_trace_unresolved() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let db = receipts_db(dir.path(), &[("hv-1", None, "https://x")]);
    let out = resolve_trace_notes(&db, Path::new("/vault"), &[]).expect("resolve");
    assert!(out.is_empty(), "unresolved traces key on themselves in aggregate()");
}

/// An unreadable receipts DB is an `Err`, never an empty map: the caller
/// overwrites the queue unconditionally, and resolving nothing silently
/// restores exactly the double-counting the reconciliation removes.
#[test]
fn an_unreadable_receipts_db_is_an_error() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let err = resolve_trace_notes(&dir.path().join("nope.db"), Path::new("/vault"), &[])
        .expect_err("a missing DB must not read as an empty map");
    assert!(format!("{err:?}").contains("receipts DB"), "{err:?}");
}

/// End to end on the frozen fixture: two traces for one note collapse, the
/// reject is suppressed, and the resolving candidate never appears.
#[test]
fn the_frozen_fixture_reproduces_the_shipped_counting_rule() {
    let dir = tempfile::tempdir().expect("tmpdir");
    write_trace(
        dir.path(),
        "ht-970437f1",
        "tags:\n  - system-prompt\n  - documentation\n  - rust\nmeta:\n  produced-at: 2026-08-05T00:00:00Z\n",
    );
    write_trace(
        dir.path(),
        "ht-ba9e3191",
        "tags:\n  - system-prompt\n  - documentation\n  - rust\nmeta:\n  produced-at: 2026-09-21T00:00:00Z\n",
    );
    write_trace(dir.path(), "ht-third", "tags:\n  - system-prompt\n  - ci-cd\n");
    write_trace(dir.path(), "ht-fourth", "tags:\n  - ci-cd\n");
    write_trace(dir.path(), "ht-fifth", "tags:\n  - ci-cd\n");

    let db = receipts_db(
        dir.path(),
        &[
            // Both recorded under the pre-classify `inbox/` location, which
            // `cortex classify` vacated: tier 2 misses and tier 3 resolves
            // them by unique basename + matching source. This is the live
            // shape - the note's own `trace:` names only the SECOND of the
            // two, which is exactly why the frontmatter join alone
            // double-counted this note.
            (
                "ht-970437f1",
                Some("/vault/inbox/pi-coding-agent-free-course.md"),
                "https://course",
            ),
            (
                "ht-ba9e3191",
                Some("/vault/inbox/pi-coding-agent-free-course.md"),
                "https://course",
            ),
            ("ht-third", Some("/vault/notes/third.md"), "https://third"),
            ("ht-fourth", Some("/vault/notes/fourth.md"), "https://fourth"),
            ("ht-fifth", Some("/vault/notes/fifth.md"), "https://fifth"),
        ],
    );
    let notes = vec![
        note(
            "notes/pi-coding-agent-free-course.md",
            Some("ht-ba9e3191"),
            Some("https://course"),
        ),
        note("notes/third.md", Some("ht-third"), Some("https://third")),
        note("notes/fourth.md", Some("ht-fourth"), Some("https://fourth")),
        note("notes/fifth.md", Some("ht-fifth"), Some("https://fifth")),
    ];

    let scan = read_staged_candidates(dir.path()).expect("scan");
    let trace_to_note = resolve_trace_notes(&db, Path::new("/vault"), &notes).expect("resolve");
    let out = aggregate(&[], &scan.candidates, &trace_to_note, &canon(), &mapping(), 3);

    let tags: Vec<&str> = out.iter().map(|p| p.tag.as_str()).collect();
    assert_eq!(tags, vec!["ci-cd"], "only ci-cd clears 3: {out:?}");
    assert!(
        !tags.contains(&"system-prompt"),
        "the M1 canary: 3 traces, but two are one note, so it scores 2"
    );
    assert!(
        !tags.contains(&"documentation"),
        "a human reject must never be proposed"
    );
    assert!(!tags.contains(&"rust"), "an exact canonical member is not a candidate");
    assert_eq!(out[0].frequency, 3);
}
