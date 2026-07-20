use super::*;
use crate::vault::Note;
use vault::search::SearchIndex;

fn note(path: &str, creator: Option<&str>, source: Option<&str>, tags: &[&str]) -> Note {
    let fm = vault::frontmatter::Frontmatter {
        creator: creator.map(|s| s.to_string()),
        source: source.map(|s| s.to_string()),
        tags: Some(tags.iter().map(|t| t.to_string()).collect()),
        ..Default::default()
    };
    Note {
        path: std::path::PathBuf::from(path),
        frontmatter: fm,
        body: String::new(),
        raw: String::new(),
    }
}

#[test]
fn slugify_handles_names_and_hosts() {
    assert_eq!(slugify("Andrej Karpathy"), "andrej-karpathy");
    assert_eq!(slugify("youtube.com"), "youtube-com");
    assert_eq!(slugify("langchain"), "langchain");
    assert_eq!(slugify("  Weird__Name!! "), "weird-name");
}

#[test]
fn collect_stubs_covers_concepts_creators_sources_and_overcap_tags() {
    let notes = vec![
        note(
            "notes/a.md",
            Some("Andrej Karpathy"),
            Some("https://www.youtube.com/watch?v=1"),
            &["llm", "rare"],
        ),
        note(
            "notes/b.md",
            Some("Andrej Karpathy"),
            Some("https://youtube.com/x"),
            &["llm"],
        ),
        note("notes/c.md", None, None, &["llm"]),
    ];
    // fanout_cap=2 -> "llm" (df=3) is over cap -> gets a tag hub; "rare" (df=1) does not.
    let stubs = collect_stubs(&["graphrag".to_string()], &["rag".to_string()], &notes, 2);
    let by_slug: std::collections::HashMap<&str, HubKind> = stubs.iter().map(|s| (s.slug.as_str(), s.kind)).collect();

    assert_eq!(by_slug.get("graphrag"), Some(&HubKind::Concept));
    assert_eq!(
        by_slug.get("rag"),
        Some(&HubKind::Concept),
        "alias target gets a concept hub"
    );
    assert_eq!(by_slug.get("andrej-karpathy"), Some(&HubKind::Creator));
    assert_eq!(by_slug.get("youtube-com"), Some(&HubKind::Source));
    assert_eq!(by_slug.get("llm"), Some(&HubKind::Tag), "over-cap tag gets a hub");
    assert!(!by_slug.contains_key("rare"), "under-cap tag gets no hub");
}

#[test]
fn write_stubs_is_idempotent_and_writes_entity_frontmatter() {
    let dir = tempfile::tempdir().expect("tmp");
    let stubs = vec![HubStub {
        slug: "langchain".to_string(),
        kind: HubKind::Concept,
        title: "langchain".to_string(),
    }];

    // Dry-run creates nothing.
    let (report, materialized) = write_stubs(dir.path(), &stubs, false, "2026-06-06").expect("dry");
    assert_eq!(report.created, 1);
    assert!(materialized.is_empty());
    assert!(!dir.path().join("entities/langchain.md").exists());

    // Apply creates the file with type: entity frontmatter.
    let (report, materialized) = write_stubs(dir.path(), &stubs, true, "2026-06-06").expect("apply");
    assert_eq!(report.created, 1);
    assert_eq!(materialized, vec!["langchain".to_string()]);
    let body = std::fs::read_to_string(dir.path().join("entities/langchain.md")).expect("read");
    assert!(
        body.contains("type: entity"),
        "frontmatter declares the entity note type"
    );
    assert!(body.contains("ontotype: technology"));

    // Second apply is idempotent: file already exists, nothing re-created.
    let (report, _) = write_stubs(dir.path(), &stubs, true, "2026-06-06").expect("apply2");
    assert_eq!(report.created, 0);
    assert_eq!(report.existing, 1);
}

#[test]
fn populate_entities_sets_hub_path_only_when_materialized() {
    let index = SearchIndex::open_memory().expect("open");
    let stubs = vec![
        HubStub {
            slug: "langchain".to_string(),
            kind: HubKind::Concept,
            title: "langchain".to_string(),
        },
        HubStub {
            slug: "rag".to_string(),
            kind: HubKind::Concept,
            title: "rag".to_string(),
        },
    ];
    // Only langchain's hub exists on disk.
    let n = populate_entities(&index, &stubs, &["langchain".to_string()]).expect("populate");
    assert_eq!(n, 2);
    assert_eq!(index.count_entities().expect("count"), 2);

    let lc = index.get_entity("langchain").expect("get").expect("present");
    assert_eq!(lc.0, "concept");
    assert_eq!(
        lc.1.as_deref(),
        Some("entities/langchain.md"),
        "materialized -> hub_path set"
    );
    assert_eq!(lc.2.as_deref(), Some("technology"));

    let rag = index.get_entity("rag").expect("get").expect("present");
    assert_eq!(rag.1, None, "not materialized -> hub_path NULL");
}

/// Phase-3 out-of-band hub deletion: a tag hub is stubbed, the graph pass
/// routes an over-cap tag's notes through it, the hub note is deleted out of
/// band (cascade clears its edges), and the next graph pass skips the
/// stale-`dst` edge without aborting — re-stubbing returns the edge.
#[test]
fn graph_skips_edges_to_out_of_band_deleted_hub() {
    use crate::config::GraphConfig;

    let mut index = SearchIndex::open_memory().expect("open");
    // 3 notes share blanket tag "llm" (df=3); cap=2 -> over cap -> hub-routed.
    for i in 0..3 {
        index
            .insert_test_note_graph(&format!("notes/{i}.md"), &["llm"], "", "", "tech", "x", 100)
            .expect("note");
    }
    // Stub the tag hub note so the graph pass can route to it.
    index
        .insert_test_note_graph("entities/llm.md", &[], "", "", "tech", "hub", 100)
        .expect("hub");

    let cfg = GraphConfig {
        min_cosine: -1.0,
        fanout_cap: 2,
        ..GraphConfig::default()
    };

    crate::graph::build(&mut index, &cfg, true).expect("build1");
    assert!(
        index.count_edges(Some("shared-tag")).expect("count") >= 3,
        "each over-cap note routes one edge to the hub"
    );

    // Delete the hub note out of band (simulate `index_vault` dropping it);
    // the edge `ON DELETE CASCADE` clears its incident edges.
    index.delete_note_for_test("entities/llm.md").expect("delete hub");
    // Stale edges to the hub were cascade-cleared.
    let after_delete = index.count_edges(Some("shared-tag")).expect("count");
    assert_eq!(after_delete, 0, "cascade cleared edges to the deleted hub");

    // Re-run the graph pass (full rebuild). The hub is gone, so the over-cap
    // tag has no hub to route to -> edges are skipped, NOT an abort/crash.
    let stats = crate::graph::build(&mut index, &cfg, true).expect("build2 must not crash");
    assert_eq!(
        index.count_edges(Some("shared-tag")).expect("count"),
        0,
        "no hub -> over-cap tag emits no edges; pass did not abort"
    );
    let _ = stats;
}

fn repo_note(path: &str, repo: &str) -> Note {
    Note {
        path: std::path::PathBuf::from(path),
        frontmatter: vault::frontmatter::Frontmatter {
            repo: Some(repo.to_string()),
            ..Default::default()
        },
        body: String::new(),
        raw: String::new(),
    }
}

#[test]
fn repo_hub_slug_is_injective_on_the_org_repo_split() {
    // The adversarial pair the generic slugify would collide (both -> a-b-c)
    // mint DISTINCT hubs via the `--` boundary (mandatory /-bearing fixture:
    // the bare-token kinds never exercise this path).
    assert_eq!(repo_hub_slug("a/b-c"), "repo-a--b-c");
    assert_eq!(repo_hub_slug("a-b/c"), "repo-a-b--c");
    assert_ne!(repo_hub_slug("a/b-c"), repo_hub_slug("a-b/c"));
    // Case-insensitive (GitHub names fold): one hub.
    assert_eq!(repo_hub_slug("Scott/Loopr"), repo_hub_slug("scott/loopr"));
    assert_eq!(repo_hub_slug("scottidler/loopr"), "repo-scottidler--loopr");
    // Accepted lossiness: `.`/`_` fold to `-`, so these merge (documented,
    // membership-only, `repo:` frontmatter stays byte-truthful).
    assert_eq!(repo_hub_slug("org/.github"), repo_hub_slug("org/github"));
}

#[test]
fn collect_stubs_mints_repo_hub_deterministically_and_disjoint_from_concepts() {
    let notes = vec![
        repo_note("a.md", "scottidler/loopr"),
        repo_note("b.md", "scottidler/loopr"),
    ];
    // A concept literally named "loopr" must NOT collide with the repo hub.
    let stubs1 = collect_stubs(&["loopr".to_string()], &[], &notes, 10);
    let stubs2 = collect_stubs(&["loopr".to_string()], &[], &notes, 10);
    assert_eq!(
        stubs1, stubs2,
        "collect_stubs is deterministic byte-for-byte across sweeps"
    );
    let slugs: Vec<&str> = stubs1.iter().map(|s| s.slug.as_str()).collect();
    assert!(slugs.contains(&"repo-scottidler--loopr"), "repo hub minted: {slugs:?}");
    assert!(
        slugs.contains(&"loopr"),
        "concept hub coexists (disjoint namespace): {slugs:?}"
    );
    assert_eq!(
        stubs1.iter().filter(|s| s.slug == "repo-scottidler--loopr").count(),
        1,
        "two same-repo notes mint ONE repo hub"
    );
    let repo_stub = stubs1
        .iter()
        .find(|s| s.slug == "repo-scottidler--loopr")
        .expect("repo stub present");
    assert_eq!(repo_stub.kind, HubKind::Repo);
    assert_eq!(repo_stub.title, "scottidler/loopr", "title stays byte-truthful");
}

#[test]
fn collect_stubs_skips_malformed_repo() {
    let notes = vec![repo_note("bad.md", "no-slash-here")];
    let stubs = collect_stubs(&[], &[], &notes, 10);
    assert!(
        !stubs.iter().any(|s| matches!(s.kind, HubKind::Repo)),
        "a malformed repo slug mints no repo hub (edge skipped, note still indexed)"
    );
}

struct OkSynth(String);
impl HubSynthesizer for OkSynth {
    fn synthesize(&self, _title: &str, _members: &[String]) -> eyre::Result<String> {
        Ok(self.0.clone())
    }
}
struct ErrSynth;
impl HubSynthesizer for ErrSynth {
    fn synthesize(&self, _title: &str, _members: &[String]) -> eyre::Result<String> {
        eyre::bail!("synthesis boom")
    }
}

#[test]
fn synthesize_hub_writes_body_preserves_frontmatter_and_is_failsafe() {
    let dir = tempfile::tempdir().expect("tmp");
    let hub = dir.path().join("repo-scottidler--loopr.md");
    let original =
        "---\ntitle: scottidler/loopr\ntype: entity\nontotype: repo\n---\n\n# scottidler/loopr\n\nstub body\n";
    std::fs::write(&hub, original).expect("seed");

    // Success: body rewritten, frontmatter preserved, same path (no re-slug).
    let out = synthesize_hub(
        &hub,
        "scottidler/loopr",
        &["a.md".to_string(), "b.md".to_string()],
        &OkSynth("These 2 notes cover the loopr work.".to_string()),
    )
    .expect("synth");
    assert_eq!(out, SynthOutcome::Synthesized);
    let after = std::fs::read_to_string(&hub).expect("read");
    assert!(
        after.starts_with("---\ntitle: scottidler/loopr\ntype: entity\nontotype: repo\n---\n"),
        "frontmatter preserved verbatim: {after}"
    );
    assert!(
        after.contains("These 2 notes cover the loopr work."),
        "body synthesized"
    );
    assert!(!after.contains("stub body"), "prior stub body replaced");
    assert!(hub.exists(), "hub not deleted/re-slugged");

    // Failure: prior body byte-identical (no write at all).
    let before = std::fs::read_to_string(&hub).expect("read");
    let out = synthesize_hub(&hub, "scottidler/loopr", &["a.md".to_string()], &ErrSynth).expect("synth");
    assert_eq!(out, SynthOutcome::Preserved);
    assert_eq!(
        std::fs::read_to_string(&hub).expect("read"),
        before,
        "a failed synthesis leaves the body byte-identical"
    );
    assert!(hub.exists(), "hub still present after a failed synthesis");
}
