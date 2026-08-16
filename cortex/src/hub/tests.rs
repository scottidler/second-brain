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

/// A note with an explicit `repo:` and/or three-state `repos-touched:`. `None`
/// leaves the field unset (key omitted); `Some(vec![])` is present-but-empty.
fn multi_repo_note(path: &str, repo: Option<&str>, repos_touched: Option<Vec<&str>>) -> Note {
    Note {
        path: std::path::PathBuf::from(path),
        frontmatter: vault::frontmatter::Frontmatter {
            repo: repo.map(|s| s.to_string()),
            repos_touched: repos_touched.map(|v| v.into_iter().map(|s| s.to_string()).collect()),
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
fn repo_hub_path_nests_and_is_injective_across_orgs() {
    // Nested folders mirror ~/repos/<org>/<repo> under entities/repos/ (Scott,
    // 2026-07-20), superseding the flat repo-<org>--<repo>.md scheme.
    assert_eq!(
        repo_hub_path("tatari-tv/okta-auth-py"),
        "entities/repos/tatari-tv/okta-auth-py.md"
    );
    // Injective on the org/repo split: the adversarial pair the flat `slugify`
    // would collide lands at DISTINCT nested paths because the `/` is now a real
    // directory boundary - no separator-encoding needed.
    assert_eq!(repo_hub_path("a/b-c"), "entities/repos/a/b-c.md");
    assert_eq!(repo_hub_path("a-b/c"), "entities/repos/a-b/c.md");
    assert_ne!(repo_hub_path("a/b-c"), repo_hub_path("a-b/c"));
    // Case-folds like the slug (GitHub names are case-insensitive) -> one hub.
    assert_eq!(repo_hub_path("Scott/Loopr"), repo_hub_path("scott/loopr"));
    // Collision-safe across orgs sharing a repo basename: distinct hub files.
    assert_ne!(
        repo_hub_path("scottidler/loopr"),
        repo_hub_path("tatari-tv/loopr"),
        "same basename, different org -> distinct nested hub files"
    );
}

#[test]
fn repo_hub_wikilink_resolves_to_nested_file_and_is_collision_safe() {
    // The wikilink TARGET is the exact vault-relative path minus `.md`, so it
    // resolves UNCONDITIONALLY in Obsidian (literal-path match), never depending
    // on basename uniqueness.
    let target = repo_hub_wikilink_target("tatari-tv/okta-auth-py");
    assert_eq!(target, "entities/repos/tatari-tv/okta-auth-py");
    assert_eq!(
        format!("{target}.md"),
        repo_hub_path("tatari-tv/okta-auth-py"),
        "target + .md is EXACTLY the on-disk hub path -> it resolves"
    );
    // Two orgs sharing a repo basename get DISTINCT resolving targets (the org
    // dir disambiguates), where a bare `[[loopr]]` would be ambiguous.
    assert_ne!(
        repo_hub_wikilink_target("scottidler/loopr"),
        repo_hub_wikilink_target("tatari-tv/loopr")
    );
}

#[test]
fn render_hub_repo_emits_resolving_nested_wikilink() {
    let stub = HubStub {
        slug: repo_hub_slug("tatari-tv/okta-auth-py"),
        kind: HubKind::Repo,
        title: "tatari-tv/okta-auth-py".to_string(),
    };
    let md = render_hub(&stub, "2026-07-20");
    // A LIVE self-link (not a backticked code span) targeting the nested file,
    // aliased to the byte-truthful <org>/<repo> for a clean render.
    assert!(
        md.contains("[[entities/repos/tatari-tv/okta-auth-py|tatari-tv/okta-auth-py]]"),
        "repo hub stub carries a resolving nested self-link: {md}"
    );
    // NOT the flat slug, which would NOT resolve to the nested file.
    assert!(
        !md.contains("[[repo-tatari-tv--okta-auth-py]]"),
        "no broken flat-slug self-link: {md}"
    );
}

#[test]
fn hub_path_nests_repo_but_keeps_other_kinds_flat() {
    let repo = HubStub {
        slug: repo_hub_slug("tatari-tv/okta-auth-py"),
        kind: HubKind::Repo,
        title: "tatari-tv/okta-auth-py".to_string(),
    };
    assert_eq!(repo.hub_path(), "entities/repos/tatari-tv/okta-auth-py.md");
    let concept = HubStub {
        slug: "langchain".to_string(),
        kind: HubKind::Concept,
        title: "langchain".to_string(),
    };
    assert_eq!(concept.hub_path(), "entities/langchain.md", "non-repo kinds stay flat");
}

#[test]
fn write_stubs_writes_repo_hub_at_nested_path() {
    let dir = tempfile::tempdir().expect("tmp");
    let stubs = vec![HubStub {
        slug: repo_hub_slug("tatari-tv/okta-auth-py"),
        kind: HubKind::Repo,
        title: "tatari-tv/okta-auth-py".to_string(),
    }];
    let (report, materialized) = write_stubs(dir.path(), &stubs, true, "2026-07-20").expect("apply");
    assert_eq!(report.created, 1);
    // The entities-table id (materialized slug) stays the injective flat slug;
    // only the FILE nests.
    assert_eq!(materialized, vec![repo_hub_slug("tatari-tv/okta-auth-py")]);
    assert!(
        dir.path().join("entities/repos/tatari-tv/okta-auth-py.md").exists(),
        "repo hub materialized at the nested path"
    );
    assert!(
        !dir.path().join("entities/repo-tatari-tv--okta-auth-py.md").exists(),
        "no flat repo hub file created"
    );
    // Idempotent: the nested file already exists on a second apply.
    let (report2, _) = write_stubs(dir.path(), &stubs, true, "2026-07-20").expect("apply2");
    assert_eq!(report2.created, 0);
    assert_eq!(report2.existing, 1);
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

#[test]
fn frozen_corpus_hub_groupings_are_deterministic_across_sweeps() {
    // Phase 13 acceptance (3f frozen-corpus determinism), extended for Phase 4
    // multi-repo: a mixed corpus (single-repo + multi-repo + creator + source +
    // over-cap tag) sweeps to byte-identical groupings twice, and the
    // multi-repo note bridges BOTH touched hubs.
    let notes = vec![
        repo_note("r1.md", "scottidler/loopr"),
        // A session touching repos X+Y: repo: loopr (also in repos-touched),
        // repos-touched adds the secondary tatari-tv/marquee bridge.
        multi_repo_note(
            "r2.md",
            Some("scottidler/loopr"),
            Some(vec!["scottidler/loopr", "tatari-tv/marquee"]),
        ),
        note(
            "c1.md",
            Some("Andrej Karpathy"),
            Some("https://youtube.com/x"),
            &["rust", "ai"],
        ),
        note(
            "c2.md",
            Some("Andrej Karpathy"),
            Some("https://youtube.com/y"),
            &["rust", "ai"],
        ),
    ];
    let a = collect_stubs(&["graphrag".to_string()], &[], &notes, 1);
    let b = collect_stubs(&["graphrag".to_string()], &[], &notes, 1);
    assert_eq!(a, b, "identical hub groupings byte-for-byte across sweeps");
    // The multi-repo note bridges: BOTH touched hubs are present, minted once.
    let slugs: Vec<&str> = a.iter().map(|s| s.slug.as_str()).collect();
    assert!(slugs.contains(&"repo-scottidler--loopr"), "primary/repo hub: {slugs:?}");
    assert!(
        slugs.contains(&"repo-tatari-tv--marquee"),
        "secondary repos-touched hub bridged, not dropped: {slugs:?}"
    );
    assert_eq!(
        a.iter().filter(|s| s.slug == "repo-scottidler--loopr").count(),
        1,
        "loopr appears in r1.repo, r2.repo AND r2.repos_touched -> exactly one hub"
    );
}

#[test]
fn collect_stubs_repos_touched_three_state_distinction_byte_for_byte() {
    // Phase 4: the None vs [] vs populated distinction, byte-for-byte at the
    // hub-minting seam. None (touched set unknowable) and Some(vec![])
    // (definitively touched nothing) BOTH mint no repo hub - identical output.
    // The populated case mints one hub per element and is DISTINCT from both.
    let none = collect_stubs(&[], &[], &[multi_repo_note("n.md", None, None)], 10);
    assert!(none.is_empty(), "None repos_touched (no repo:) mints nothing: {none:?}");

    let empty = collect_stubs(&[], &[], &[multi_repo_note("e.md", None, Some(vec![]))], 10);
    assert_eq!(
        empty, none,
        "Some(vec![]) is byte-for-byte identical to None at the hub seam (no hubs)"
    );

    let populated = collect_stubs(
        &[],
        &[],
        &[multi_repo_note(
            "p.md",
            None,
            Some(vec!["scottidler/loopr", "tatari-tv/marquee"]),
        )],
        10,
    );
    let slugs: Vec<&str> = populated.iter().map(|s| s.slug.as_str()).collect();
    assert!(slugs.contains(&"repo-scottidler--loopr"), "X hub minted: {slugs:?}");
    assert!(slugs.contains(&"repo-tatari-tv--marquee"), "Y hub minted: {slugs:?}");
    assert_eq!(populated.len(), 2, "exactly the two touched-repo hubs");
    assert_ne!(
        populated, none,
        "populated repos_touched mints hubs that None/[] do not"
    );
}

#[test]
fn collect_stubs_mints_every_touched_repo_deduped_against_repo() {
    // Phase 4: repo: X + repos-touched [X, Y] -> hubs for X and Y, with X
    // (present in BOTH) minted exactly once. Deterministic across sweeps.
    let notes = vec![multi_repo_note(
        "s.md",
        Some("scottidler/loopr"),
        Some(vec!["scottidler/loopr", "tatari-tv/marquee"]),
    )];
    let a = collect_stubs(&[], &[], &notes, 10);
    let b = collect_stubs(&[], &[], &notes, 10);
    assert_eq!(a, b, "collect_stubs is deterministic byte-for-byte across sweeps");
    let repo_slugs: Vec<&str> = a
        .iter()
        .filter(|s| s.kind == HubKind::Repo)
        .map(|s| s.slug.as_str())
        .collect();
    assert!(repo_slugs.contains(&"repo-scottidler--loopr"), "X hub: {repo_slugs:?}");
    assert!(repo_slugs.contains(&"repo-tatari-tv--marquee"), "Y hub: {repo_slugs:?}");
    assert_eq!(
        a.iter().filter(|s| s.slug == "repo-scottidler--loopr").count(),
        1,
        "X in both repo: and repos-touched mints ONE hub (deduped on slug)"
    );

    // A malformed repos-touched element skips (edge dropped, note still
    // indexed); the well-formed sibling still mints its hub.
    let mixed = collect_stubs(
        &[],
        &[],
        &[multi_repo_note(
            "m.md",
            None,
            Some(vec!["no-slash-here", "tatari-tv/marquee"]),
        )],
        10,
    );
    let mixed_slugs: Vec<&str> = mixed.iter().map(|s| s.slug.as_str()).collect();
    assert!(
        mixed_slugs.contains(&"repo-tatari-tv--marquee"),
        "well-formed sibling minted despite the malformed entry: {mixed_slugs:?}"
    );
    assert_eq!(
        mixed.len(),
        1,
        "the malformed entry mints nothing; only the valid hub remains"
    );
}

#[test]
fn hub_membership_is_monotonic_additions_only() {
    // Phase 13 acceptance (monotonicity): adding notes only ADDS stubs; every
    // previously-collected stub survives unchanged (no move, no removal).
    let base = vec![repo_note("r1.md", "scottidler/loopr")];
    let stubs_before = collect_stubs(&[], &[], &base, 10);

    let grown = vec![
        repo_note("r1.md", "scottidler/loopr"),
        repo_note("r2.md", "tatari-tv/marquee"),
        note("c.md", Some("New Creator"), None, &[]),
    ];
    let stubs_after = collect_stubs(&[], &[], &grown, 10);

    for s in &stubs_before {
        assert!(
            stubs_after.contains(s),
            "previously-assigned stub {s:?} must survive unchanged after growth (no move/removal)"
        );
    }
    assert!(stubs_after.len() > stubs_before.len(), "growth adds stubs");
}

// --- entity-hub-two-vector-synthesis Phase 1 -------------------------------

/// The divergence-killer. Before this phase the hub side and the graph side each
/// carried their own host parser with DIFFERENT signatures, so the hub minted
/// nothing exactly where the graph produced a bucket key. Now a Source stub's
/// on-disk path and the `source-member` edge's `dst` come from one seam, and
/// this pins them equal on the shapes that actually differ (`www.`, query
/// string, uppercase, port, deep path).
#[test]
fn source_hub_path_matches_stub_hub_path() {
    let urls = [
        "https://www.youtube.com/watch?v=abc",
        "https://youtube.com/x",
        "http://Example.COM/deep/path?q=1",
        "https://every.to/chain-of-thought",
        "https://localhost:8080/x",
    ];
    for url in urls {
        let host = source_host(url).unwrap_or_else(|| panic!("host for {url}"));
        let stub = HubStub {
            slug: slugify(&host),
            kind: HubKind::Source,
            title: host,
        };
        assert_eq!(
            Some(stub.hub_path()),
            source_hub_path(url),
            "the stub path and the edge dst must be byte-identical for {url}"
        );
    }
}

/// Schemeless input is the contract's `None`: `collect_stubs` cannot mint those
/// hubs, so `source_hub_path` must refuse to name one. `clyde://` is the only
/// non-http scheme in the corpus (261 session notes); the rest are provenance
/// markers, not publishers.
#[test]
fn source_hub_path_is_none_for_schemeless_and_hostless_input() {
    for value in [
        "",
        "clyde://0f3c1a2b-4d5e-6f70-8192-a3b4c5d6e7f8",
        "pais-migration",
        "youtube-transcript",
        "https://",
    ] {
        assert_eq!(source_hub_path(value), None, "{value:?} names no source hub");
        assert_eq!(source_host(value), None, "{value:?} has no host");
    }
}

/// `source_hub_path` produces a FLAT `entities/<slug>.md`, the same namespace
/// Concept/Creator/Tag hubs share (deliberately: one hub per subject). Repo hubs
/// are the only nested kind.
#[test]
fn source_hub_path_is_flat_under_the_hub_dir() {
    assert_eq!(
        source_hub_path("https://www.youtube.com/watch?v=1").as_deref(),
        Some("entities/youtube-com.md"),
    );
}

// --- entity-hub-two-vector-synthesis Phase 2 -------------------------------

/// The live refusal marker: 134 hub bodies carry it today, all `quality=medium`,
/// so oracle's stub filter passes them and serves them as search results.
const REFUSAL: &str = "I don't have access to the actual content of those files";

fn concept_stub(slug: &str) -> HubStub {
    HubStub {
        slug: slug.to_string(),
        kind: HubKind::Concept,
        title: slug.to_string(),
    }
}

/// Write a hub note with an explicit body (and optional extra frontmatter keys).
fn seed_hub(vault: &Path, slug: &str, extra_fm: &str, body: &str) -> std::path::PathBuf {
    let rel = format!("{HUB_DIR}/{slug}.md");
    let abs = vault.join(&rel);
    std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdir");
    let content = format!(
        "---\ntitle: {slug}\ntype: entity\nontotype: technology\ndate: 2026-06-06\ntags: []\n{extra_fm}---\n\n# {slug}\n\n{body}\n"
    );
    std::fs::write(&abs, content).expect("seed hub");
    abs
}

/// Write a member note carrying a real `## Claims` section.
fn seed_member(vault: &Path, rel: &str, note_type: &str, date: &str, claims: &[&str]) {
    let abs = vault.join(rel);
    std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdir");
    let bullets: String = claims.iter().map(|c| format!("- {c}\n")).collect();
    let content = format!(
        "---\ntitle: {title}\ntype: {note_type}\ndate: {date}\n---\n\n# {title}\n\n## Claims\n\n{bullets}",
        title = rel.trim_end_matches(".md").rsplit('/').next().unwrap_or(rel),
    );
    std::fs::write(&abs, content).expect("seed member");
}

/// Register a note row in the index so `insert_edges`' resolve-or-skip rule
/// accepts edges touching it.
fn index_note(index: &SearchIndex, path: &str) {
    index
        .insert_test_note_graph(path, &[], "", "", "tech", "b", 100)
        .expect("index note");
}

fn stats_of(pass: &BodyPass) -> HubReport {
    let mut report = HubReport::default();
    report.apply_body_stats(pass);
    report
}

#[test]
fn hub_body_renders_deliberate_membership_only_and_is_idempotent() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    let stub = concept_stub("claude");
    let hub_rel = "entities/claude.md";
    let (_r, _m) = write_stubs(vault, std::slice::from_ref(&stub), true, "2026-06-06").expect("stub");

    seed_member(
        vault,
        "knowledge/tech/context-rot.md",
        "article",
        "2026-05-01",
        &["Long contexts degrade recall past 60k tokens"],
    );
    seed_member(
        vault,
        "sessions/oracle-work.md",
        "session",
        "2026-08-01",
        &["Vector-only retrieval beat equal-weight hybrid"],
    );
    seed_member(
        vault,
        "knowledge/tech/noise.md",
        "article",
        "2026-07-01",
        &["inferred noise"],
    );
    seed_hub(vault, "agents", "", "## Summary\n\nanother hub body\n");
    seed_member(
        vault,
        "entities/agents.md",
        "entity",
        "2026-07-01",
        &["hub-to-hub claim"],
    );

    let mut index = SearchIndex::open_memory().expect("open");
    for p in [
        hub_rel,
        "knowledge/tech/context-rot.md",
        "sessions/oracle-work.md",
        "knowledge/tech/noise.md",
        "entities/agents.md",
    ] {
        index_note(&index, p);
    }
    index
        .insert_edges(&[
            vault::search::Edge::deterministic("knowledge/tech/context-rot.md", hub_rel, "wikilink", 1.0),
            vault::search::Edge::deterministic("sessions/oracle-work.md", hub_rel, "repo-member", 1.0),
            // Inferred, not deliberate.
            vault::search::Edge::deterministic("knowledge/tech/noise.md", hub_rel, "semantic", 0.9),
            // A hub linking a hub: would feed generated bodies back into hubs.
            vault::search::Edge::deterministic("entities/agents.md", hub_rel, "wikilink", 1.0),
        ])
        .expect("edges");

    let caps = RenderConfig::default();
    let pass = build_hub_bodies(vault, &index, std::slice::from_ref(&stub), &caps).expect("build");
    let report = stats_of(&pass);
    assert_eq!(report.bodies_written, 1, "{report:?}");
    assert_eq!(report.members_skipped, 0);

    let body = std::fs::read_to_string(vault.join(hub_rel)).expect("read hub");
    assert!(body.contains("## From sources"), "{body}");
    assert!(body.contains("## From your sessions"), "{body}");
    assert!(
        body.contains("Long contexts degrade recall past 60k tokens ([[knowledge/tech/context-rot|context-rot]])"),
        "claim TEXT + member wikilink: {body}"
    );
    assert!(
        body.contains("Vector-only retrieval beat equal-weight hybrid"),
        "{body}"
    );
    assert!(!body.contains("inferred noise"), "semantic membership excluded: {body}");
    assert!(!body.contains("hub-to-hub claim"), "entities/% src excluded: {body}");
    assert!(
        body.starts_with("---\ntitle: claude\ntype: entity\nontotype: technology\ndate: 2026-06-06\ntags: []\n---\n"),
        "frontmatter preserved verbatim: {body}"
    );

    // Second run, unchanged inputs: zero bytes written.
    let pass2 = build_hub_bodies(vault, &index, &[stub], &caps).expect("build2");
    let report2 = stats_of(&pass2);
    assert_eq!(report2.bodies_written, 0, "{report2:?}");
    assert_eq!(report2.bodies_unchanged, 1, "{report2:?}");
    assert_eq!(
        std::fs::read_to_string(vault.join(hub_rel)).expect("read hub"),
        body,
        "a re-run with unchanged inputs is byte-identical"
    );
}

#[test]
fn refusal_and_stale_rendered_bodies_reset_while_a_stub_is_kept_byte_identical() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    let refusal_hub = seed_hub(vault, "getvoibe-com", "", REFUSAL);
    let stale_hub = seed_hub(
        vault,
        "terraform",
        "",
        "## Summary\n\nterraform: hub of 1 source.\nSources: gone\n\n## From sources\n\n- gone ([[k/gone|gone]])",
    );
    let (_r, _m) = write_stubs(vault, &[concept_stub("langchain")], true, "2026-06-06").expect("stub");
    let untouched_stub = std::fs::read_to_string(vault.join("entities/langchain.md")).expect("read");

    let index = SearchIndex::open_memory().expect("open");
    for p in [
        "entities/getvoibe-com.md",
        "entities/terraform.md",
        "entities/langchain.md",
    ] {
        index_note(&index, p);
    }

    let stubs = vec![
        concept_stub("getvoibe-com"),
        concept_stub("terraform"),
        concept_stub("langchain"),
    ];
    let pass = build_hub_bodies(vault, &index, &stubs, &RenderConfig::default()).expect("build");
    let report = stats_of(&pass);
    assert_eq!(report.bodies_reset, 2, "refusal + stale render both reset: {report:?}");
    assert_eq!(report.stubs_kept, 1, "an existing stub is left alone: {report:?}");

    let after_refusal = std::fs::read_to_string(&refusal_hub).expect("read");
    assert!(!after_refusal.contains(REFUSAL), "refusal body gone: {after_refusal}");
    assert!(
        after_refusal.contains("Auto-stubbed by `sb cortex hub`"),
        "reset to the stub sentence: {after_refusal}"
    );
    let after_stale = std::fs::read_to_string(&stale_hub).expect("read");
    assert!(
        !after_stale.contains("## From sources"),
        "a rendered body whose claim-bearing members are gone is reset: {after_stale}"
    );
    assert_eq!(
        std::fs::read_to_string(vault.join("entities/langchain.md")).expect("read"),
        untouched_stub,
        "a hub already carrying the stub keeps it byte-identical"
    );
}

#[test]
fn a_run_where_every_member_is_unreadable_preserves_every_body() {
    // "nothing to say" and "could not find out" are different conditions: an IO
    // error never licenses a reset. A vault-root misconfig makes EVERY member
    // unreadable at once, which is exactly the mass-reset hazard this branch
    // closes - so the fixture is a whole run of them, not one hub.
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    let mut index = SearchIndex::open_memory().expect("open");
    let mut before = Vec::new();
    let mut stubs = Vec::new();
    let mut edges = Vec::new();
    for slug in ["claude", "agents", "rag"] {
        let hub = seed_hub(
            vault,
            slug,
            "",
            &format!("## Summary\n\n{slug}: hub of 1 source.\nSources: prior\n"),
        );
        before.push((hub.clone(), std::fs::read_to_string(&hub).expect("read")));
        let hub_rel = format!("entities/{slug}.md");
        let member_rel = format!("knowledge/tech/absent-{slug}.md");
        index_note(&index, &hub_rel);
        index_note(&index, &member_rel); // indexed, but NOT on disk
        edges.push(vault::search::Edge::deterministic(member_rel, hub_rel, "wikilink", 1.0));
        stubs.push(concept_stub(slug));
    }
    index.insert_edges(&edges).expect("edges");

    let pass = build_hub_bodies(vault, &index, &stubs, &RenderConfig::default()).expect("build");
    let report = stats_of(&pass);
    assert_eq!(report.bodies_preserved, 3, "{report:?}");
    assert_eq!(report.bodies_reset, 0, "an error is never a reset: {report:?}");
    assert_eq!(
        report.members_skipped, 3,
        "the skipped members are reported: {report:?}"
    );
    for (path, content) in &before {
        assert_eq!(
            &std::fs::read_to_string(path).expect("read"),
            content,
            "every body is byte-identical after a member-load failure"
        );
    }
}

#[test]
fn the_run_level_backstop_aborts_before_any_write() {
    // The failure branch 2 cannot see: `parse_body_claims` is infallible, so a
    // claim-parse regression "succeeds" everywhere with zero claims and would
    // stub the whole hub layer in one silent pass.
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    let rendered =
        "## Summary\n\nx: hub of 1 source.\nSources: prior claim\n\n## From sources\n\n- prior claim ([[k/a|a]])";
    let slugs = ["a", "b", "c"];
    let mut before = Vec::new();
    let index = SearchIndex::open_memory().expect("open");
    for slug in slugs {
        let path = seed_hub(vault, slug, "", rendered);
        before.push((path.clone(), std::fs::read_to_string(&path).expect("read")));
        index_note(&index, &format!("entities/{slug}.md"));
    }
    let stubs: Vec<HubStub> = slugs.iter().map(|s| concept_stub(s)).collect();

    let caps = RenderConfig {
        max_render_resets_per_run: 2,
        ..RenderConfig::default()
    };
    let err = build_hub_bodies(vault, &index, &stubs, &caps).expect_err("backstop must abort");
    let msg = format!("{err:#}");
    assert!(msg.contains("max-render-resets-per-run"), "{msg}");
    assert!(msg.contains("entities/a.md"), "the abort names the hubs: {msg}");
    for (path, content) in &before {
        assert_eq!(
            &std::fs::read_to_string(path).expect("read"),
            content,
            "nothing is written when the backstop trips"
        );
    }

    // Raising the max lets the intended resets through.
    let caps = RenderConfig {
        max_render_resets_per_run: 3,
        ..RenderConfig::default()
    };
    let pass = build_hub_bodies(vault, &index, &stubs, &caps).expect("build");
    assert_eq!(stats_of(&pass).bodies_reset, 3);
}

#[test]
fn refusal_resets_do_not_count_against_the_backstop() {
    // The first live run resets ~124 refusal bodies; only PREVIOUSLY RENDERED
    // bodies (first H2 `## Summary`) are the regression signal.
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    let index = SearchIndex::open_memory().expect("open");
    let mut stubs = Vec::new();
    for slug in ["r1", "r2", "r3"] {
        seed_hub(vault, slug, "", REFUSAL);
        index_note(&index, &format!("entities/{slug}.md"));
        stubs.push(concept_stub(slug));
    }
    let caps = RenderConfig {
        max_render_resets_per_run: 0,
        ..RenderConfig::default()
    };
    let pass = build_hub_bodies(vault, &index, &stubs, &caps).expect("refusal resets are expected, not a regression");
    assert_eq!(stats_of(&pass).bodies_reset, 3);
}

#[test]
fn manual_bodies_are_never_rewritten_while_hub_synthesized_ones_are() {
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    let manual = seed_hub(vault, "manual-hub", "hub-body: manual\n", "A body Scott wrote by hand.");
    let manual_before = std::fs::read_to_string(&manual).expect("read");
    let synthesized = seed_hub(
        vault,
        "claude",
        "hub-synthesized: 2026-07-02\n",
        "## ONE SENTENCE SUMMARY:\n\nFabric boilerplate.",
    );

    seed_member(vault, "k/a.md", "article", "2026-05-01", &["a real source claim"]);
    let mut index = SearchIndex::open_memory().expect("open");
    for p in ["entities/manual-hub.md", "entities/claude.md", "k/a.md"] {
        index_note(&index, p);
    }
    index
        .insert_edges(&[
            vault::search::Edge::deterministic("k/a.md", "entities/manual-hub.md", "wikilink", 1.0),
            vault::search::Edge::deterministic("k/a.md", "entities/claude.md", "wikilink", 1.0),
        ])
        .expect("edges");

    let stubs = vec![concept_stub("manual-hub"), concept_stub("claude")];
    let pass = build_hub_bodies(vault, &index, &stubs, &RenderConfig::default()).expect("build");
    let report = stats_of(&pass);
    assert_eq!(report.bodies_manual, 1, "{report:?}");
    assert_eq!(report.bodies_written, 1, "{report:?}");
    assert_eq!(
        std::fs::read_to_string(&manual).expect("read"),
        manual_before,
        "`hub-body: manual` is never touched"
    );
    let after = std::fs::read_to_string(&synthesized).expect("read");
    assert!(
        !after.contains("ONE SENTENCE SUMMARY"),
        "a 2026-07-02 Fabric body is builder-owned and gets overwritten: {after}"
    );
    assert!(after.contains("a real source claim"), "{after}");
    assert!(
        after.contains("hub-synthesized: 2026-07-02"),
        "the provenance stamp survives in the preserved frontmatter: {after}"
    );
}

#[test]
fn plan_hub_body_covers_the_four_branches() {
    let raw = "---\ntitle: t\ntype: entity\n---\n\n# t\n\nold body\n";
    let stub = "stub sentence";

    let manual_raw = "---\ntitle: t\nhub-body: manual\n---\n\n# t\n\nhand written\n";
    assert_eq!(
        plan_hub_body(manual_raw, "t", Some("## Summary\n\nnew"), stub, false).outcome,
        SynthOutcome::Manual
    );

    let preserved = plan_hub_body(raw, "t", None, stub, true);
    assert_eq!(preserved.outcome, SynthOutcome::Preserved);
    assert!(preserved.content.is_none(), "a preserved hub writes nothing");

    let rendered = plan_hub_body(raw, "t", Some("## Summary\n\nnew"), stub, false);
    assert_eq!(rendered.outcome, SynthOutcome::Rendered);
    let content = rendered.content.expect("content");
    assert!(
        content.starts_with("---\ntitle: t\ntype: entity\n---\n"),
        "frontmatter verbatim: {content}"
    );
    assert!(content.contains("# t\n\n## Summary\n\nnew\n"), "{content}");
    // Byte-identical re-render writes nothing.
    assert_eq!(
        plan_hub_body(&content, "t", Some("## Summary\n\nnew"), stub, false).outcome,
        SynthOutcome::Unchanged
    );

    let reset = plan_hub_body(raw, "t", None, stub, false);
    assert_eq!(reset.outcome, SynthOutcome::Reset);
    let content = reset.content.expect("content");
    assert!(content.ends_with("# t\n\nstub sentence\n"), "{content}");
    assert_eq!(
        plan_hub_body(&content, "t", None, stub, false).outcome,
        SynthOutcome::StubKept
    );

    // No frontmatter block: never overwritten.
    let plan = plan_hub_body("# t\n\nbody with no frontmatter\n", "t", Some("x"), stub, false);
    assert_eq!(plan.outcome, SynthOutcome::Preserved);
    assert!(plan.content.is_none());
}

#[test]
fn body_is_rendered_keys_on_the_first_h2() {
    assert!(body_is_rendered("---\ntitle: t\n---\n\n# t\n\n## Summary\n\nx\n"));
    assert!(!body_is_rendered(
        "---\ntitle: t\n---\n\n# t\n\nAuto-stubbed by `sb cortex hub`.\n"
    ));
    assert!(!body_is_rendered(&format!("---\ntitle: t\n---\n\n# t\n\n{REFUSAL}\n")));
    assert!(
        !body_is_rendered("# t\n\n## ONE SENTENCE SUMMARY:\n\nFabric output\n"),
        "a Fabric body is not a rendered body"
    );
}

#[test]
fn load_hub_member_reads_type_date_and_claims_and_errors_when_absent() {
    let dir = tempfile::tempdir().expect("tmp");
    seed_member(
        dir.path(),
        "k/a.md",
        "youtube",
        "2026-04-01",
        &["claim one", "claim two"],
    );
    let member = load_hub_member(dir.path(), "k/a.md").expect("load");
    assert_eq!(member.path, "k/a.md");
    assert_eq!(member.title, "a");
    assert_eq!(member.note_type, "youtube");
    assert_eq!(member.date.as_deref(), Some("2026-04-01"));
    assert_eq!(
        member.claims.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
        vec!["claim one", "claim two"]
    );
    assert!(
        load_hub_member(dir.path(), "k/missing.md").is_err(),
        "a missing member is an ERROR, not an empty member"
    );
}

#[test]
fn dry_run_writes_no_vault_bytes_and_records_no_entities() {
    // Truth in naming: `populate_entities` used to upsert oracle's `entities`
    // table whenever the index opened, regardless of --apply. A dry run now
    // never opens the index at all.
    let dir = tempfile::tempdir().expect("tmp");
    let vault = dir.path();
    std::fs::create_dir_all(vault.join("knowledge")).expect("mkdir");
    std::fs::write(
        vault.join("knowledge/a.md"),
        "---\ntitle: A\ntype: article\ncreator: Andrej Karpathy\n---\n\n# A\n",
    )
    .expect("note");

    let config = crate::config::Config::default();
    let opts = crate::opts::HubOpts {
        apply: false,
        synthesize: true,
        asymmetry: false,
    };
    let report = run(vault, &config, &opts).expect("dry run");
    assert!(report.created > 0, "the dry run still REPORTS what it would stub");
    assert_eq!(report.entities_recorded, 0, "no entities-table upsert on a dry run");
    assert_eq!(report.bodies_written, 0);
    assert!(!vault.join("entities").exists(), "a dry run creates no hub files");
}

#[test]
fn no_fabric_call_is_reachable_from_cortex_hub() {
    // Zero LLM calls in the hub pipeline is a design invariant, not a habit:
    // `FabricHubSynthesizer` prompted the `summarize` pattern with a bare list
    // of member PATHS, per hub, unbounded - which is how 134 hub bodies became
    // the literal model refusal. Asserted structurally against the module's own
    // source so a future edit cannot quietly reintroduce the call.
    for (name, src) in [
        ("hub.rs", include_str!("../hub.rs")),
        ("hub/render.rs", include_str!("render.rs")),
    ] {
        for needle in ["fabric::", "run_pattern", "truncate_input", "HubSynthesizer"] {
            assert!(!src.contains(needle), "cortex/src/{name} must not reference {needle}");
        }
        // Every hub write is atomic: this pass rewrites hundreds of files on a
        // Syncthing'd vault, where a torn write propagates to every machine.
        assert!(
            !src.contains("fs::write("),
            "cortex/src/{name} must write hub notes via vault::note::write_atomic, never fs::write"
        );
    }
}
