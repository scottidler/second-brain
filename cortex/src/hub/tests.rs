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

/// A synthesizer that records whether it was invoked, so a test can prove the
/// memberless guard short-circuits BEFORE any LLM call.
struct CountingSynth {
    calls: std::cell::Cell<usize>,
    body: String,
}
impl HubSynthesizer for CountingSynth {
    fn synthesize(&self, _title: &str, _members: &[String]) -> eyre::Result<String> {
        self.calls.set(self.calls.get() + 1);
        Ok(self.body.clone())
    }
}

#[test]
fn synthesize_hub_skips_llm_when_memberless() {
    // Regression: a repo hub with ZERO wired members must NOT be sent to the
    // LLM. Live, feeding an empty member list produced a hallucinated body
    // ("no member notes were provided") on the tatari-tv/okta-auth-py hub. The
    // fail-safe leaves the stub body byte-intact and never calls the synth.
    let dir = tempfile::tempdir().expect("tmp");
    let hub = dir.path().join("okta-auth-py.md");
    let stub_body = "---\ntitle: tatari-tv/okta-auth-py\ntype: entity\nontotype: repo\n---\n\n# tatari-tv/okta-auth-py\n\nstub body only\n";
    std::fs::write(&hub, stub_body).expect("seed");

    let synth = CountingSynth {
        calls: std::cell::Cell::new(0),
        body: "no member notes were provided (hallucination)".to_string(),
    };
    let out = synthesize_hub(&hub, "tatari-tv/okta-auth-py", &[], &synth).expect("synth");

    assert_eq!(
        out,
        SynthOutcome::Preserved,
        "zero members -> preserve, never synthesize"
    );
    assert_eq!(
        synth.calls.get(),
        0,
        "the LLM synthesizer is NEVER invoked for a memberless hub"
    );
    assert_eq!(
        std::fs::read_to_string(&hub).expect("read"),
        stub_body,
        "stub body left byte-identical (no hallucinated overwrite)"
    );
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

#[test]
fn synthesize_hub_never_modifies_member_notes() {
    // Phase 13 acceptance (immutability): synthesizing a hub touches ONLY the
    // hub file - member notes are byte-identical after.
    let dir = tempfile::tempdir().expect("tmp");
    let hub = dir.path().join("repo-x--y.md");
    std::fs::write(&hub, "---\ntitle: x/y\ntype: entity\n---\n\nstub\n").expect("hub");
    let member = dir.path().join("m.md");
    let member_content = "---\ntitle: M\ntype: session\n---\nmember body\n";
    std::fs::write(&member, member_content).expect("member");
    let members = vec![member.to_string_lossy().to_string()];
    synthesize_hub(&hub, "x/y", &members, &OkSynth("synthesized body".to_string())).expect("synth");
    assert_eq!(
        std::fs::read_to_string(&member).expect("read"),
        member_content,
        "a member note is byte-identical after hub synthesis (membership never mutates the note)"
    );
}
