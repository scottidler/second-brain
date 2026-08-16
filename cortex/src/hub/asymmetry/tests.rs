use super::*;
use crate::hub::{HubKind, write_stubs};
use vault::search::{Edge, SearchIndex};

fn concept_stub(slug: &str) -> HubStub {
    HubStub {
        slug: slug.to_string(),
        kind: HubKind::Concept,
        title: slug.to_string(),
    }
}

/// Write a member note of a given vector-bearing `type:`. No claims needed -
/// the asymmetry report counts membership, not claim-bearing members.
fn seed_member(vault: &Path, rel: &str, note_type: &str) {
    let abs = vault.join(rel);
    std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdir");
    let title = rel.trim_end_matches(".md").rsplit('/').next().unwrap_or(rel);
    std::fs::write(
        &abs,
        format!("---\ntitle: {title}\ntype: {note_type}\ndate: 2026-06-01\n---\n\n# {title}\n\nbody\n"),
    )
    .expect("seed member");
}

fn index_note(index: &SearchIndex, path: &str) {
    index
        .insert_test_note_graph(path, &[], "", "", "tech", "b", 100)
        .expect("index note");
}

#[test]
fn classify_covers_all_four_buckets() {
    assert_eq!(AsymmetryBucket::classify(2, 3), AsymmetryBucket::Both);
    assert_eq!(AsymmetryBucket::classify(2, 0), AsymmetryBucket::LearnedNotApplied);
    assert_eq!(AsymmetryBucket::classify(0, 3), AsymmetryBucket::AppliedNotRead);
    assert_eq!(AsymmetryBucket::classify(0, 0), AsymmetryBucket::Unlinked);
}

#[test]
fn bucket_names_match_the_design_doc_exactly() {
    assert_eq!(AsymmetryBucket::Both.as_str(), "both");
    assert_eq!(AsymmetryBucket::LearnedNotApplied.as_str(), "learned-not-applied");
    assert_eq!(AsymmetryBucket::AppliedNotRead.as_str(), "applied-not-read");
    assert_eq!(AsymmetryBucket::Unlinked.as_str(), "unlinked");
}

/// The end-to-end classification test: four hubs, one per bucket, built from
/// real deliberate/inferred edges over a real `SearchIndex`. Pins that
/// `semantic` (inferred) and an `entities/%` src (hub-to-hub) are excluded
/// exactly as `hub_members_deliberate` excludes them for the body builder,
/// and that the four buckets sum to the hub count (the success criterion).
#[test]
fn build_asymmetry_report_classifies_deliberate_membership_only() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();

    let stubs = vec![
        concept_stub("both-hub"),
        concept_stub("learned-hub"),
        concept_stub("applied-hub"),
        concept_stub("unlinked-hub"),
    ];
    let (_r, _m) = write_stubs(vault, &stubs, true, "2026-06-06").expect("stubs");

    seed_member(vault, "k/source-a.md", "article");
    seed_member(vault, "k/session-a.md", "session");
    seed_member(vault, "k/inferred.md", "article");
    seed_member(vault, "k/source-b.md", "youtube");
    seed_member(vault, "k/session-b.md", "session");

    let mut index = SearchIndex::open_memory().expect("open");
    for p in [
        "entities/both-hub.md",
        "entities/learned-hub.md",
        "entities/applied-hub.md",
        "entities/unlinked-hub.md",
        "k/source-a.md",
        "k/session-a.md",
        "k/inferred.md",
        "k/source-b.md",
        "k/session-b.md",
    ] {
        index_note(&index, p);
    }
    index
        .insert_edges(&[
            // both-hub: one source + one session, both deliberate.
            Edge::deterministic("k/source-a.md", "entities/both-hub.md", "wikilink", 1.0),
            Edge::deterministic("k/session-a.md", "entities/both-hub.md", "repo-member", 1.0),
            // Inferred edge into both-hub must NOT count toward either vector.
            Edge::deterministic("k/inferred.md", "entities/both-hub.md", "semantic", 0.9),
            // A hub-to-hub wikilink must NOT count (entities/% src excluded).
            Edge::deterministic("entities/applied-hub.md", "entities/both-hub.md", "wikilink", 1.0),
            // learned-hub: source only.
            Edge::deterministic("k/source-b.md", "entities/learned-hub.md", "source-member", 1.0),
            // applied-hub: session only.
            Edge::deterministic("k/session-b.md", "entities/applied-hub.md", "creator-member", 1.0),
            // unlinked-hub: no deliberate edges at all.
        ])
        .expect("edges");

    let report = build_asymmetry_report(vault, &stubs, &index).expect("report");
    assert_eq!(report.rows.len(), 4, "{:?}", report.rows);

    let by_path: std::collections::HashMap<&str, &AsymmetryRow> =
        report.rows.iter().map(|r| (r.hub_path.as_str(), r)).collect();

    let both = by_path["entities/both-hub.md"];
    assert_eq!(both.sources, 1, "{both:?}");
    assert_eq!(both.sessions, 1, "{both:?}");
    assert_eq!(both.bucket, AsymmetryBucket::Both, "{both:?}");

    let learned = by_path["entities/learned-hub.md"];
    assert_eq!(learned.sources, 1);
    assert_eq!(learned.sessions, 0);
    assert_eq!(learned.bucket, AsymmetryBucket::LearnedNotApplied);

    let applied = by_path["entities/applied-hub.md"];
    assert_eq!(applied.sources, 0);
    assert_eq!(applied.sessions, 1);
    assert_eq!(applied.bucket, AsymmetryBucket::AppliedNotRead);

    let unlinked = by_path["entities/unlinked-hub.md"];
    assert_eq!(unlinked.sources, 0);
    assert_eq!(unlinked.sessions, 0);
    assert_eq!(unlinked.bucket, AsymmetryBucket::Unlinked);

    // The success criterion: the four buckets sum to the hub count.
    let totals = report.totals();
    assert_eq!(totals.total(), report.rows.len());
    assert_eq!(totals.both, 1);
    assert_eq!(totals.learned_not_applied, 1);
    assert_eq!(totals.applied_not_read, 1);
    assert_eq!(totals.unlinked, 1);
}

/// A stub whose hub file was never materialized has no membership to
/// measure and is excluded from the report entirely (not counted as
/// `unlinked`) - mirrors the body builder's `abs.exists()` gate.
#[test]
fn unmaterialized_stub_is_excluded_from_the_report() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    let stubs = vec![concept_stub("never-materialized")];
    let index = SearchIndex::open_memory().expect("open");
    let report = build_asymmetry_report(vault, &stubs, &index).expect("report");
    assert!(report.rows.is_empty(), "{:?}", report.rows);
}

/// An unreadable member (indexed but missing on disk) is skipped, logged, and
/// never aborts the report - the hub still lands in a bucket from whatever
/// membership DID load.
#[test]
fn an_unreadable_member_is_skipped_not_fatal() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    let stub = concept_stub("partial-hub");
    let (_r, _m) = write_stubs(vault, std::slice::from_ref(&stub), true, "2026-06-06").expect("stub");
    seed_member(vault, "k/present.md", "article");

    let mut index = SearchIndex::open_memory().expect("open");
    index_note(&index, "entities/partial-hub.md");
    index_note(&index, "k/present.md");
    index_note(&index, "k/missing.md"); // indexed, but NOT written to disk
    index
        .insert_edges(&[
            Edge::deterministic("k/present.md", "entities/partial-hub.md", "wikilink", 1.0),
            Edge::deterministic("k/missing.md", "entities/partial-hub.md", "wikilink", 1.0),
        ])
        .expect("edges");

    let report = build_asymmetry_report(vault, std::slice::from_ref(&stub), &index).expect("report");
    assert_eq!(report.rows.len(), 1);
    assert_eq!(report.rows[0].sources, 1, "the missing member is skipped, not counted");
    assert_eq!(report.rows[0].bucket, AsymmetryBucket::LearnedNotApplied);
}

/// Two runs against unchanged state produce byte-identical output (the
/// second success criterion).
#[test]
fn two_runs_produce_byte_identical_output() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    let stub = concept_stub("claude");
    let (_r, _m) = write_stubs(vault, std::slice::from_ref(&stub), true, "2026-06-06").expect("stub");
    seed_member(vault, "k/source.md", "article");
    seed_member(vault, "k/session.md", "session");

    let mut index = SearchIndex::open_memory().expect("open");
    for p in ["entities/claude.md", "k/source.md", "k/session.md"] {
        index_note(&index, p);
    }
    index
        .insert_edges(&[
            Edge::deterministic("k/source.md", "entities/claude.md", "wikilink", 1.0),
            Edge::deterministic("k/session.md", "entities/claude.md", "repo-member", 1.0),
        ])
        .expect("edges");

    let stubs = std::slice::from_ref(&stub);
    let first = build_asymmetry_report(vault, stubs, &index).expect("first");
    let second = build_asymmetry_report(vault, stubs, &index).expect("second");
    assert_eq!(
        first, second,
        "unchanged inputs produce a structurally identical report"
    );
    assert_eq!(
        first.render(),
        second.render(),
        "unchanged inputs render byte-identical text"
    );
    assert_eq!(first.rows[0].bucket, AsymmetryBucket::Both);
}

/// Read-only, asserted by test: building the report neither writes a vault
/// file nor changes a row in the index (notes/edges/entities counts and the
/// vault directory's file bytes are unchanged before vs. after).
#[test]
fn asymmetry_report_writes_nothing_to_vault_or_index() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    let stubs = vec![concept_stub("claude"), concept_stub("stub-only")];
    // "stub-only" is left unmaterialized on purpose (exercises the exists()
    // skip inside a run that also has a real hub).
    let (_r, _m) = write_stubs(vault, std::slice::from_ref(&stubs[0]), true, "2026-06-06").expect("stub");
    seed_member(vault, "k/source.md", "article");
    seed_member(vault, "k/session.md", "session");

    let mut index = SearchIndex::open_memory().expect("open");
    for p in ["entities/claude.md", "k/source.md", "k/session.md"] {
        index_note(&index, p);
    }
    index
        .insert_edges(&[
            Edge::deterministic("k/source.md", "entities/claude.md", "wikilink", 1.0),
            Edge::deterministic("k/session.md", "entities/claude.md", "repo-member", 1.0),
        ])
        .expect("edges");

    // Snapshot every vault file's bytes.
    let snapshot = |root: &Path| -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut out = std::collections::BTreeMap::new();
        for entry in walkdir::WalkDir::new(root) {
            let entry = entry.expect("walk");
            if entry.file_type().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .expect("rel")
                    .to_string_lossy()
                    .to_string();
                out.insert(rel, std::fs::read(entry.path()).expect("read"));
            }
        }
        out
    };
    let vault_before = snapshot(vault);
    let notes_before = index.count_notes().expect("count_notes");
    let edges_before = index.count_edges(None).expect("count_edges");
    let entities_before = index.count_entities().expect("count_entities");

    let _report = build_asymmetry_report(vault, &stubs, &index).expect("report");

    assert_eq!(snapshot(vault), vault_before, "no vault file changed");
    assert_eq!(
        index.count_notes().expect("count_notes"),
        notes_before,
        "notes table untouched"
    );
    assert_eq!(
        index.count_edges(None).expect("count_edges"),
        edges_before,
        "edges table untouched"
    );
    assert_eq!(
        index.count_entities().expect("count_entities"),
        entities_before,
        "entities table untouched"
    );
}

/// Structural guard (mirrors `no_fabric_call_is_reachable_from_cortex_hub`):
/// no write primitive is reachable from this module's own source, so a future
/// edit cannot quietly turn the report into a writer.
#[test]
fn asymmetry_report_is_read_only_by_construction() {
    let src = include_str!("../asymmetry.rs");
    for needle in [
        "fs::write(",
        "write_atomic",
        "INSERT INTO",
        "upsert_entity",
        ".execute(",
    ] {
        assert!(
            !src.contains(needle),
            "cortex/src/hub/asymmetry.rs must not reference {needle}"
        );
    }
}
