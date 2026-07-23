use super::*;
use crate::config::GraphConfig;
use vault::embedding::{EmbeddingModel, MockEmbedder};
use vault::search::{EmbeddingKind, SearchIndex};

/// A test GraphConfig with a low fan-out cap so we can exercise the cap with
/// a handful of notes.
fn cfg() -> GraphConfig {
    GraphConfig {
        min_cosine: -1.0, // admit all so tests don't depend on mock geometry
        fanout_cap: 3,
        ..GraphConfig::default()
    }
}

/// Upsert a summary embedding for `path` and pin the active model to the mock.
fn embed(index: &SearchIndex, m: &MockEmbedder, path: &str, text: &str, modified_at: i64) {
    let v = m.embed_one(text).expect("embed");
    index
        .upsert_embedding(
            path,
            EmbeddingKind::Summary,
            0,
            text,
            &v,
            m.model_version(),
            modified_at,
        )
        .expect("upsert");
}

fn set_active_mock(index: &mut SearchIndex, m: &MockEmbedder) {
    // active_model defaults to bge-small; bump to the mock so the semantic
    // reader sees the rows we wrote.
    index
        .set_active_embedding(m.model_version(), m.dim())
        .expect("set active");
}

#[test]
fn full_rebuild_builds_semantic_edges() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(16, "mock-graph-v1");
    index
        .insert_test_note_graph("notes/a.md", &[], "", "", "tech", "alpha", 100)
        .expect("a");
    index
        .insert_test_note_graph("notes/b.md", &[], "", "", "tech", "beta", 100)
        .expect("b");
    set_active_mock(&mut index, &m);
    embed(&index, &m, "notes/a.md", "shared topic", 100);
    embed(&index, &m, "notes/b.md", "shared topic", 100);

    let stats = build(&mut index, &cfg(), true).expect("build");
    assert!(stats.full_rebuild);
    assert!(
        stats.semantic >= 2,
        "semantic edges built both directions; got {}",
        stats.semantic
    );
    assert_eq!(
        index.count_edges(Some("semantic")).expect("count"),
        stats.semantic as i64
    );
}

/// The load-bearing stranding regression: a note skipped for a missing
/// embedding must get its semantic edges once the embedding lands, WITHOUT
/// `notes.modified_at` ever being bumped (embed does not touch it).
#[test]
fn semantic_edge_not_stranded_when_embedding_lands_after_skip() {
    let mut index = SearchIndex::open_memory().expect("open");
    let m = MockEmbedder::new(16, "mock-graph-v1");
    // Two notes; only `b` is embedded at first. `a` has NO embedding.
    index
        .insert_test_note_graph("notes/a.md", &[], "", "", "tech", "alpha topic", 100)
        .expect("a");
    index
        .insert_test_note_graph("notes/b.md", &[], "", "", "tech", "alpha topic", 100)
        .expect("b");
    set_active_mock(&mut index, &m);
    embed(&index, &m, "notes/b.md", "alpha topic", 100);

    // First pass: full rebuild. `a` has no embedding -> no semantic edge owned
    // by `a`.
    let s1 = build(&mut index, &cfg(), false).expect("build1");
    assert!(s1.full_rebuild, "first run with no last_run_at is a full rebuild");
    let a_edges_1 = index
        .expand_graph(&["notes/a.md".to_string()], 1, Some(&["semantic".to_string()]), -1.0)
        .expect("e1");
    assert!(
        a_edges_1.iter().all(|r| r.path != "notes/b.md") || a_edges_1.is_empty(),
        "a owns no semantic edge yet"
    );
    let a_owned_before = index.count_edges(Some("semantic")).expect("count");

    // Embedding for `a` lands LATER, WITHOUT bumping notes.modified_at.
    // (upsert_embedding sets produced_at = now; modified_at stays 100.)
    embed(&index, &m, "notes/a.md", "alpha topic", 100);

    // Second pass: incremental. Keyed on produced_at, `a` is now a semantic
    // target and gets its edges — not stranded.
    let s2 = build(&mut index, &cfg(), false).expect("build2");
    assert!(!s2.full_rebuild, "second run is incremental");
    let a_edges_2 = index
        .expand_graph(&["notes/a.md".to_string()], 1, Some(&["semantic".to_string()]), -1.0)
        .expect("e2");
    assert!(
        a_edges_2.iter().any(|r| r.path == "notes/b.md"),
        "a's semantic edge to b exists after its embedding landed"
    );
    assert!(
        index.count_edges(Some("semantic")).expect("count") > a_owned_before,
        "more semantic edges after the embedding landed"
    );
}

#[test]
fn dangling_wikilink_is_skipped_not_inserted() {
    let mut index = SearchIndex::open_memory().expect("open");
    // `a` links to a real note and a dangling one.
    index
        .insert_test_note_graph("notes/a.md", &[], "", "", "tech", "see [[b]] and [[ghost]]", 100)
        .expect("a");
    index
        .insert_test_note_graph("notes/b.md", &[], "", "", "tech", "body", 100)
        .expect("b");

    let stats = build(&mut index, &cfg(), true).expect("build");
    // Only the resolved [[b]] edge exists; [[ghost]] dangled and was skipped.
    assert_eq!(
        index.count_edges(Some("wikilink")).expect("count"),
        1,
        "only resolved wikilink"
    );
    assert_eq!(stats.wikilink, 1);
}

#[test]
fn shared_tag_rarity_downweights_blanket_tags() {
    let mut index = SearchIndex::open_memory().expect("open");
    // `rare` shared by a,b only (df=2). `common` shared by a,b (df=2 here) —
    // keep small so both fire, then assert the rare contributes more weight.
    index
        .insert_test_note_graph("notes/a.md", &["rare", "common"], "", "", "tech", "x", 100)
        .expect("a");
    index
        .insert_test_note_graph("notes/b.md", &["rare", "common"], "", "", "tech", "y", 100)
        .expect("b");
    // Pad `common` to df=3 (still under cap) so its rarity weight is lower.
    index
        .insert_test_note_graph("notes/c.md", &["common"], "", "", "tech", "z", 100)
        .expect("c");

    build(&mut index, &cfg(), true).expect("build");
    // a->b shared-tag weight = 1/ln(1+2) [rare, df=2] + 1/ln(1+3) [common, df=3].
    let neighbors = index
        .expand_graph(&["notes/a.md".to_string()], 1, Some(&["shared-tag".to_string()]), 0.0)
        .expect("e");
    let ab = neighbors
        .iter()
        .find(|r| r.path == "notes/b.md")
        .expect("a-b shared-tag edge");
    let expected = 1.0_f32 / 3.0_f32.ln() + 1.0 / 4.0_f32.ln();
    assert!(
        (ab.weight - expected).abs() < 1e-5,
        "rarity-weighted sum; got {} want {}",
        ab.weight,
        expected
    );
}

#[test]
fn shared_tag_skips_buckets_over_fanout_cap() {
    let mut index = SearchIndex::open_memory().expect("open");
    // 5 notes all sharing tag `blanket`; cap is 3 -> bucket skipped entirely.
    for i in 0..5 {
        index
            .insert_test_note_graph(&format!("notes/{i}.md"), &["blanket"], "", "", "tech", "x", 100)
            .expect("note");
    }
    build(&mut index, &cfg(), true).expect("build");
    assert_eq!(
        index.count_edges(Some("shared-tag")).expect("count"),
        0,
        "blanket tag bucket over fan-out cap emits no pairwise edges"
    );
}

#[test]
fn shared_creator_edges_built_with_fixed_weight() {
    let mut index = SearchIndex::open_memory().expect("open");
    index
        .insert_test_note_graph("notes/a.md", &[], "", "alice", "tech", "x", 100)
        .expect("a");
    index
        .insert_test_note_graph("notes/b.md", &[], "", "alice", "tech", "y", 100)
        .expect("b");
    build(&mut index, &cfg(), true).expect("build");
    let neighbors = index
        .expand_graph(
            &["notes/a.md".to_string()],
            1,
            Some(&["shared-creator".to_string()]),
            0.0,
        )
        .expect("e");
    let ab = neighbors.iter().find(|r| r.path == "notes/b.md").expect("creator edge");
    assert!((ab.weight - 0.2).abs() < 1e-6);
}

#[test]
fn shared_source_uses_host_not_full_url() {
    let mut index = SearchIndex::open_memory().expect("open");
    index
        .insert_test_note_graph(
            "notes/a.md",
            &[],
            "https://www.youtube.com/watch?v=1",
            "",
            "tech",
            "x",
            100,
        )
        .expect("a");
    index
        .insert_test_note_graph("notes/b.md", &[], "https://youtube.com/watch?v=2", "", "tech", "y", 100)
        .expect("b");
    build(&mut index, &cfg(), true).expect("build");
    // Both normalize to host youtube.com -> a shared-source edge forms.
    let neighbors = index
        .expand_graph(
            &["notes/a.md".to_string()],
            1,
            Some(&["shared-source".to_string()]),
            0.0,
        )
        .expect("e");
    assert!(
        neighbors.iter().any(|r| r.path == "notes/b.md"),
        "host-normalized source edge"
    );
}

#[test]
fn incremental_only_rebuilds_changed_notes() {
    let mut index = SearchIndex::open_memory().expect("open");
    index
        .insert_test_note_graph("notes/a.md", &["t"], "", "", "tech", "x", 100)
        .expect("a");
    index
        .insert_test_note_graph("notes/b.md", &["t"], "", "", "tech", "y", 100)
        .expect("b");
    build(&mut index, &cfg(), true).expect("full");
    let before = index.count_edges(None).expect("count");

    // Add a new note c (modified_at higher than the content watermark).
    index
        .insert_test_note_graph("notes/c.md", &["t"], "", "", "tech", "z", 500)
        .expect("c");
    let stats = build(&mut index, &cfg(), false).expect("incr");
    assert!(!stats.full_rebuild);
    assert_eq!(stats.notes_processed, 1, "only the new note c is reprocessed");
    assert!(index.count_edges(None).expect("count") > before, "c's edges added");
}

/// Index a session note carrying `repo:` + `repos-touched` through the real
/// frontmatter -> upsert -> `graph_note_rows` path, so the edge builder reads
/// the same shape production does.
fn index_repo_session(index: &SearchIndex, path: &str, repo: Option<&str>, repos_touched: Option<Vec<&str>>) {
    use std::path::PathBuf;
    use vault::frontmatter::Frontmatter;
    use vault::note::Note;
    let note = Note {
        path: PathBuf::from(path),
        frontmatter: Frontmatter {
            title: Some("session".to_string()),
            note_type: Some("session".to_string()),
            origin: Some("generated".to_string()),
            repo: repo.map(|s| s.to_string()),
            repos_touched: repos_touched.map(|v| v.into_iter().map(|s| s.to_string()).collect()),
            ..Frontmatter::default()
        },
        body: "session body".to_string(),
        raw: "---\n---\nbody".to_string(),
    };
    index.index_one(&note, 100).expect("index session");
}

/// Phase 4 positive: a session touching repos X+Y joins BOTH repo hubs via
/// deterministic repo-member edges, and the `repo:` anchor that ALSO appears in
/// `repos-touched` collapses to ONE edge (deduped on the resolved hub path). The
/// hub notes must exist at their nested paths for the edges to resolve.
#[test]
fn multi_repo_member_edges_join_every_touched_repo_hub_deduped() {
    let mut index = SearchIndex::open_memory().expect("open");
    let repos = ["scottidler/loopr", "tatari-tv/marquee", "scottidler/second-brain"];
    // Stub the three repo hubs at their real nested paths so `insert_edges`
    // resolves the `dst` (an absent hub note would silently drop the edge).
    // Empty domain so the hubs do not form shared-domain edges among themselves
    // (which would pollute `hub_members`, a kind-agnostic incoming-edge query).
    for repo in repos {
        let hub = crate::hub::repo_hub_path(repo);
        index
            .insert_test_note_graph(&hub, &[], "", "", "", "hub", 100)
            .expect("hub note");
    }
    // repo: loopr, repos-touched [loopr, marquee, second-brain] -> loopr is
    // duplicated across `repo` and `repos_touched` and must collapse to one edge.
    index_repo_session(
        &index,
        "inbox/multi.md",
        Some("scottidler/loopr"),
        Some(vec!["scottidler/loopr", "tatari-tv/marquee", "scottidler/second-brain"]),
    );

    build(&mut index, &cfg(), true).expect("build");

    assert_eq!(
        index.count_edges(Some("repo-member")).expect("count"),
        3,
        "three distinct touched hubs -> three repo-member edges, the duplicated loopr collapsed"
    );
    for repo in repos {
        let hub = crate::hub::repo_hub_path(repo);
        assert_eq!(
            index.hub_members(&hub).expect("members"),
            vec!["inbox/multi.md".to_string()],
            "the session joins the {repo} hub"
        );
    }
}

/// Phase 4 negative: with `repos-touched` absent (`None`) or present-empty
/// (`Some(vec![])`), only the single `repo:` anchor yields a repo-member edge -
/// no phantom multi-repo edges. Proves `None` and `[]` behave identically to a
/// bare `repo:` (the three-state's edge-layer collapse).
#[test]
fn no_extra_repo_member_edges_when_repos_touched_absent_or_empty() {
    let mut index = SearchIndex::open_memory().expect("open");
    let hub = crate::hub::repo_hub_path("scottidler/loopr");
    index
        .insert_test_note_graph(&hub, &[], "", "", "tech", "hub", 100)
        .expect("hub note");
    index_repo_session(&index, "inbox/none.md", Some("scottidler/loopr"), None);
    index_repo_session(&index, "inbox/empty.md", Some("scottidler/loopr"), Some(vec![]));

    build(&mut index, &cfg(), true).expect("build");

    assert_eq!(
        index.count_edges(Some("repo-member")).expect("count"),
        2,
        "each note yields exactly ONE repo-member edge (to the loopr hub); no phantom edges"
    );
    assert_eq!(
        index.hub_members(&hub).expect("members"),
        vec!["inbox/empty.md".to_string(), "inbox/none.md".to_string()],
        "both notes join the single loopr hub and nothing else"
    );
}

/// Phase 6 (harvest-completion): extends the Phase 4 determinism harness from a
/// single full-rebuild snapshot to the INCREMENTAL path - the shape every real
/// nightly `cortex graph` run takes after the first full rebuild. Growing the
/// corpus with a NEW multi-repo note must only ADD repo-member edges; a prior
/// sweep's edge for an untouched note must never be dropped.
#[test]
fn repo_member_edges_are_monotonic_across_incremental_builds() {
    let mut index = SearchIndex::open_memory().expect("open");
    let loopr_hub = crate::hub::repo_hub_path("scottidler/loopr");
    let marquee_hub = crate::hub::repo_hub_path("tatari-tv/marquee");
    for hub in [&loopr_hub, &marquee_hub] {
        index
            .insert_test_note_graph(hub, &[], "", "", "", "hub", 100)
            .expect("hub note");
    }

    // Sweep 1 (full rebuild): note A joins repo X (loopr) only.
    index_repo_session(&index, "inbox/a.md", Some("scottidler/loopr"), None);
    let stats1 = build(&mut index, &cfg(), true).expect("build1");
    assert!(stats1.full_rebuild);
    assert_eq!(index.count_edges(Some("repo-member")).expect("count"), 1);
    assert_eq!(
        index.hub_members(&loopr_hub).expect("members"),
        vec!["inbox/a.md".to_string()]
    );

    // Sweep 2 (incremental): a NEW note B joins repos X AND Y. Note A's
    // modified_at watermark is unchanged, so the incremental pass does not
    // even reprocess it - its edge survives purely because nothing removed
    // it, not because it was rebuilt identically.
    index_repo_session(
        &index,
        "inbox/b.md",
        Some("scottidler/loopr"),
        Some(vec!["scottidler/loopr", "tatari-tv/marquee"]),
    );
    let stats2 = build(&mut index, &cfg(), false).expect("build2");
    assert!(!stats2.full_rebuild, "second pass is incremental");
    assert_eq!(stats2.notes_processed, 1, "only the new note B is reprocessed");

    assert_eq!(
        index.hub_members(&loopr_hub).expect("members"),
        vec!["inbox/a.md".to_string(), "inbox/b.md".to_string()],
        "prior member A survives; growth only ADDS B, never removes A"
    );
    assert_eq!(
        index.hub_members(&marquee_hub).expect("members"),
        vec!["inbox/b.md".to_string()],
        "the new multi-repo bridge is wired without disturbing the X hub"
    );
    assert_eq!(
        index.count_edges(Some("repo-member")).expect("count"),
        3,
        "1 prior (A->X) + 2 new (B->X, B->Y); nothing removed across the incremental sweep"
    );
}
