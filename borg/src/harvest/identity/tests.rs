#![allow(clippy::unwrap_used)]
use super::*;
use tempfile::TempDir;
use vault::receipts::ReceiptKind;
use vault::schema::Method;

/// Write a minimal harvest-shaped note at `rel` under `root`, creating parent
/// directories as needed. Returns the absolute path. Any of `trace`, `source`,
/// `hash` (`harvest-body-hash:`), `superseded_by` left `None` omits that key
/// entirely (never emits an empty/null value) so legacy-note and tombstone
/// fixtures are exact.
fn write_note(
    root: &Path,
    rel: &str,
    trace: Option<&str>,
    source: Option<&str>,
    hash: Option<&str>,
    superseded_by: Option<&str>,
) -> PathBuf {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut fm = String::from("---\ntitle: \"Test Note\"\n");
    if let Some(t) = trace {
        fm.push_str(&format!("trace: {t}\n"));
    }
    if let Some(s) = source {
        fm.push_str(&format!("source: \"{s}\"\n"));
    }
    if let Some(h) = hash {
        fm.push_str(&format!("harvest-body-hash: {h}\n"));
    }
    if let Some(sb) = superseded_by {
        fm.push_str(&format!("superseded-by: {sb}\n"));
    }
    fm.push_str("---\n\nbody\n");
    std::fs::write(&path, fm).unwrap();
    path
}

const TRACE: &str = "hv-aaaaaaaa";
const SOURCE: &str = "clyde://session-1";
const HASH: &str = "deadbeef00";

// --- Branch 1: receipts fast path -------------------------------------------------

#[test]
fn resolves_via_receipts_fast_path() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let abs = write_note(root, "notes/landed.md", Some(TRACE), Some(SOURCE), Some(HASH), None);

    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, TRACE, Method::Harvest, ReceiptKind::Session, SOURCE).unwrap();
    receipts::mark_succeeded(&conn, TRACE, abs.to_str().unwrap(), false).unwrap();

    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay)
        .unwrap()
        .expect("must resolve via receipts");
    assert_eq!(resolved, abs);
    assert!(resolved.is_absolute());
}

// --- Branch 2: vault index (receipts stale/absent) --------------------------------

#[test]
fn resolves_via_vault_index_when_receipts_points_at_a_gone_path() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // The real note now lives under notes/ (cortex moved it); receipts still
    // points at the old inbox/ path, which no longer exists.
    let real = write_note(root, "notes/moved.md", Some(TRACE), Some(SOURCE), Some(HASH), None);

    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, TRACE, Method::Harvest, ReceiptKind::Session, SOURCE).unwrap();
    let stale_path = root.join("inbox/moved.md");
    receipts::mark_succeeded(&conn, TRACE, stale_path.to_str().unwrap(), false).unwrap();

    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay)
        .unwrap()
        .expect("must fall through to the vault index");
    assert_eq!(resolved, real);
}

// --- Branch 3: crash-recovery fallback (NewNote only) ------------------------------

#[test]
fn resolves_via_crash_recovery_fallback_on_source_and_hash_match() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // A note landed under a PRIOR trace whose watermark entry was lost - the
    // resolve call below is for a brand new trace over the SAME transcript
    // (same source + same body hash), so step 3 must find it.
    let lost_run_trace = "hv-11111111";
    let abs = write_note(
        root,
        "notes/crash-recovered.md",
        Some(lost_run_trace),
        Some(SOURCE),
        Some(HASH),
        None,
    );

    let conn = receipts::open_memory().unwrap();
    let fresh_trace = "hv-22222222";
    let resolved = resolve_prior_note(&conn, root, fresh_trace, SOURCE, HASH, ResolveIntent::NewNote)
        .unwrap()
        .expect("must resolve via the crash-recovery fallback");
    assert_eq!(resolved, abs);
}

#[test]
fn crash_recovery_fallback_never_fires_under_replay_intent() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let lost_run_trace = "hv-11111111";
    write_note(
        root,
        "notes/crash-recovered.md",
        Some(lost_run_trace),
        Some(SOURCE),
        Some(HASH),
        None,
    );
    let conn = receipts::open_memory().unwrap();
    let fresh_trace = "hv-22222222";
    // Replay intent gets steps 1-2 only - step 3 is NewNote-only.
    let resolved = resolve_prior_note(&conn, root, fresh_trace, SOURCE, HASH, ResolveIntent::Replay).unwrap();
    assert!(resolved.is_none());
}

#[test]
fn crash_recovery_fallback_requires_the_hash_key_present_not_just_absent() {
    // Data Model: "both keys are required; a note lacking the hash is not
    // eligible" - a legacy note with NO harvest-body-hash key must never
    // satisfy step 3, even though it shares the source.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(root, "notes/legacy.md", Some("hv-legacyid"), Some(SOURCE), None, None);
    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, "hv-99999999", SOURCE, HASH, ResolveIntent::NewNote).unwrap();
    assert!(
        resolved.is_none(),
        "a hash-less note must never satisfy the crash-recovery fallback"
    );
}

// --- Branch 4: miss -----------------------------------------------------------------

#[test]
fn miss_returns_none_when_nothing_matches() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(
        root,
        "notes/unrelated.md",
        Some("hv-ffffffff"),
        Some("clyde://other"),
        None,
        None,
    );
    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::NewNote).unwrap();
    assert!(resolved.is_none());
}

// --- Intent gate ---------------------------------------------------------------------

#[test]
fn follow_up_intent_never_resolves_even_when_every_key_matches() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let abs = write_note(root, "notes/landed.md", Some(TRACE), Some(SOURCE), Some(HASH), None);
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(&conn, TRACE, Method::Harvest, ReceiptKind::Session, SOURCE).unwrap();
    receipts::mark_succeeded(&conn, TRACE, abs.to_str().unwrap(), false).unwrap();

    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::FollowUp).unwrap();
    assert!(
        resolved.is_none(),
        "FollowUp never resolves, even with a perfect trace+source+hash match (guards against --force)"
    );
}

// --- Three-term confirmation guard ---------------------------------------------------

#[test]
fn guard_rejects_trace_match_with_mismatched_source() {
    let fm = fm_with(Some(TRACE), Some("clyde://different-session"), Some(HASH));
    assert!(!guard_passes(&fm, TRACE, SOURCE, HASH));
}

#[test]
fn guard_rejects_mismatched_body_hash() {
    let fm = fm_with(Some(TRACE), Some(SOURCE), Some("wrong-hash"));
    assert!(!guard_passes(&fm, TRACE, SOURCE, HASH));
}

#[test]
fn guard_accepts_when_hash_key_is_absent_legacy() {
    let fm = fm_with(Some(TRACE), Some(SOURCE), None);
    assert!(guard_passes(&fm, TRACE, SOURCE, HASH));
}

#[test]
fn guard_rejects_mismatched_trace() {
    let fm = fm_with(Some("hv-different"), Some(SOURCE), Some(HASH));
    assert!(!guard_passes(&fm, TRACE, SOURCE, HASH));
}

fn fm_with(trace: Option<&str>, source: Option<&str>, hash: Option<&str>) -> Frontmatter {
    let mut extra = HashMap::new();
    if let Some(h) = hash {
        extra.insert(
            HARVEST_BODY_HASH_KEY.to_string(),
            serde_yaml::Value::String(h.to_string()),
        );
    }
    Frontmatter {
        trace: trace.map(str::to_string),
        source: source.map(str::to_string),
        extra,
        ..Default::default()
    }
}

// --- Tombstone follower ---------------------------------------------------------------

#[test]
fn tombstone_is_followed_to_its_survivor() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(root, "notes/survivor.md", None, None, None, None);
    // The resolved note itself carries superseded-by - trace/source/hash are
    // kept intact by cortex's merge executor (only slug is stripped).
    let tombstone = write_note(
        root,
        "notes/tombstoned.md",
        Some(TRACE),
        Some(SOURCE),
        Some(HASH),
        Some("survivor"),
    );
    assert!(tombstone.exists());

    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay)
        .unwrap()
        .expect("must follow the tombstone to its survivor");
    assert_eq!(resolved, root.join("notes/survivor.md"));
}

#[test]
fn tombstone_chain_is_followed_transitively() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(root, "notes/final-survivor.md", None, None, None, None);
    write_note(root, "notes/mid.md", None, None, None, Some("final-survivor"));
    let start = write_note(
        root,
        "notes/start.md",
        Some(TRACE),
        Some(SOURCE),
        Some(HASH),
        Some("mid"),
    );
    assert!(start.exists());

    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay)
        .unwrap()
        .expect("must follow a two-hop chain to the final survivor");
    assert_eq!(resolved, root.join("notes/final-survivor.md"));
}

#[test]
fn tombstone_ambiguous_stem_refuses() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // Two DIFFERENT live notes coincidentally share the filename stem "dup".
    write_note(root, "notes/dup.md", None, None, None, None);
    write_note(root, "system/dup.md", None, None, None, None);
    let start = write_note(
        root,
        "notes/start.md",
        Some(TRACE),
        Some(SOURCE),
        Some(HASH),
        Some("dup"),
    );
    assert!(start.exists());

    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay).unwrap();
    assert!(
        resolved.is_none(),
        "an ambiguous stem with >1 live candidate refuses, not guesses"
    );
}

#[test]
fn tombstone_missing_stem_refuses() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let start = write_note(
        root,
        "notes/start.md",
        Some(TRACE),
        Some(SOURCE),
        Some(HASH),
        Some("ghost-stem-that-does-not-exist"),
    );
    assert!(start.exists());

    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay).unwrap();
    assert!(resolved.is_none(), "a superseded-by stem with no matching file refuses");
}

#[test]
fn tombstone_cycle_refuses() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // a -> b -> a: a genuine cycle.
    write_note(root, "notes/b.md", None, None, None, Some("a"));
    let start = write_note(root, "notes/a.md", Some(TRACE), Some(SOURCE), Some(HASH), Some("b"));
    assert!(start.exists());

    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay).unwrap();
    assert!(
        resolved.is_none(),
        "a superseded-by cycle refuses rather than looping forever"
    );
}

#[test]
fn tombstone_depth_bound_exceeded_refuses() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // t1 -> t2 -> ... -> t8 -> t9 (never created - the chain is deliberately
    // one hop longer than MAX_TOMBSTONE_DEPTH allows, so the bound trips
    // before a live survivor is ever reached).
    for i in 1..=8 {
        let next = if i == 8 { "t9".to_string() } else { format!("t{}", i + 1) };
        write_note(root, &format!("notes/t{i}.md"), None, None, None, Some(&next));
    }
    let start = write_note(
        root,
        "notes/start.md",
        Some(TRACE),
        Some(SOURCE),
        Some(HASH),
        Some("t1"),
    );
    assert!(start.exists());

    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay).unwrap();
    assert!(resolved.is_none(), "a chain longer than the depth bound refuses");
}

#[test]
fn tombstone_tie_break_skips_tombstones_among_ambiguous_ties() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    // Two files share the stem "dup2": one is ITSELF a tombstone (pointing
    // elsewhere, irrelevant here), the other is the live intended target.
    write_note(root, "notes/dup2.md", None, None, None, None); // live
    write_note(root, "system/dup2.md", None, None, None, Some("some-other-note")); // tombstone
    let start = write_note(
        root,
        "notes/start.md",
        Some(TRACE),
        Some(SOURCE),
        Some(HASH),
        Some("dup2"),
    );
    assert!(start.exists());

    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay)
        .unwrap()
        .expect("the tie-break must skip the tombstoned duplicate and land on the live one");
    assert_eq!(resolved, root.join("notes/dup2.md"));
}

#[test]
fn tombstone_ambiguous_when_all_ties_are_tombstones() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(root, "notes/dup3.md", None, None, None, Some("elsewhere-a"));
    write_note(root, "system/dup3.md", None, None, None, Some("elsewhere-b"));
    let start = write_note(
        root,
        "notes/start.md",
        Some(TRACE),
        Some(SOURCE),
        Some(HASH),
        Some("dup3"),
    );
    assert!(start.exists());

    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay).unwrap();
    assert!(
        resolved.is_none(),
        "a stem where every tie is itself a tombstone has no live target - refuse"
    );
}

// --- Absolute paths + re-stat --------------------------------------------------------

#[test]
fn every_returned_path_is_absolute() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write_note(root, "notes/landed.md", Some(TRACE), Some(SOURCE), Some(HASH), None);
    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay)
        .unwrap()
        .expect("resolves");
    assert!(resolved.is_absolute());
}

#[test]
fn stale_vault_index_entry_fails_restat_and_falls_through() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let abs = write_note(root, "notes/landed.md", Some(TRACE), Some(SOURCE), Some(HASH), None);
    let conn = receipts::open_memory().unwrap();

    // First call builds and memoizes the index with the file present.
    let first = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay).unwrap();
    assert_eq!(first, Some(abs.clone()));

    // The file is removed WITHOUT going through `note_published` (simulating
    // an external deletion/move the index was never told about).
    std::fs::remove_file(&abs).unwrap();

    // The cached index still lists the (now-gone) path; re-stat must catch
    // this and fall through to a miss rather than returning a dead path.
    let second = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay).unwrap();
    assert!(
        second.is_none(),
        "a stale index entry that fails re-stat falls through to None"
    );
}

// --- Self-insert on write -------------------------------------------------------------

#[test]
fn note_published_self_inserts_into_the_live_index() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let conn = receipts::open_memory().unwrap();

    // Build the (empty) index for this root first, exactly as a real run
    // would before its first publish.
    let miss = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay).unwrap();
    assert!(miss.is_none());

    // Publish happens: the note lands on disk AND self-inserts into the
    // already-built in-memory index, without a rebuild.
    let abs = write_note(root, "notes/fresh.md", Some(TRACE), Some(SOURCE), Some(HASH), None);
    note_published(root, TRACE, &abs);

    let resolved = resolve_prior_note(&conn, root, TRACE, SOURCE, HASH, ResolveIntent::Replay).unwrap();
    assert_eq!(
        resolved,
        Some(abs),
        "the self-inserted entry must resolve without a rebuild"
    );
}

// --- Trace-width independence (widening 24 -> 32 bits) ---------------------------------

#[test]
fn legacy_six_hex_trace_id_still_resolves() {
    // The resolver treats a trace purely as an opaque string - a pre-widening
    // 6-hex id must resolve exactly like an 8-hex one.
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let legacy_trace = "hv-e5d240";
    let abs = write_note(
        root,
        "notes/legacy.md",
        Some(legacy_trace),
        Some(SOURCE),
        Some(HASH),
        None,
    );
    let conn = receipts::open_memory().unwrap();
    let resolved = resolve_prior_note(&conn, root, legacy_trace, SOURCE, HASH, ResolveIntent::Replay)
        .unwrap()
        .expect("a legacy 6-hex trace id resolves the same as any other opaque string");
    assert_eq!(resolved, abs);
}
