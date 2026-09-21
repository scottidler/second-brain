use std::collections::HashSet;

use super::*;
use crate::testutil::NoteBuilder;
use distillers::tags::{Deterministic, TagMethod};

/// The vocabulary the classify tests classify against. Small on purpose: every
/// tag here is one a fixture below actually carries.
fn test_canon() -> CanonicalSet {
    CanonicalSet {
        all: ["rust", "cli", "ai", "llm", "work", "programming"]
            .into_iter()
            .map(String::from)
            .collect(),
        no_segment: HashSet::new(),
        no_classifier: HashSet::new(),
        max_per_note: 7,
    }
}

/// `test_canon()` with the named tags on its protect list.
fn protecting_canon(protected: &[&str]) -> CanonicalSet {
    CanonicalSet {
        no_classifier: protected.iter().map(|t| (*t).to_string()).collect(),
        ..test_canon()
    }
}

/// A classifier that answers with whatever the test asked for, or errors.
/// Reports `ClassifierDev` so a test can tell its answer apart from the
/// `Deterministic` fallback's in `cortex-classified-by`.
struct Stub {
    tags: Vec<String>,
    confidence: Confidence,
    fail: bool,
}

impl Stub {
    fn answering(tags: &[&str], confidence: Confidence) -> Box<dyn TagClassifier> {
        Box::new(Self {
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            confidence,
            fail: false,
        })
    }

    fn failing() -> Box<dyn TagClassifier> {
        Box::new(Self {
            tags: Vec::new(),
            confidence: Confidence::Low,
            fail: true,
        })
    }
}

impl TagClassifier for Stub {
    fn classify(&self, _input: &TagInput) -> Result<TagOutput> {
        if self.fail {
            return Err(eyre::eyre!("stub classifier failure"));
        }
        Ok(TagOutput {
            tags: self.tags.clone(),
            scores: None,
            confidence: self.confidence,
            method: TagMethod::ClassifierDev,
        })
    }

    fn method(&self) -> TagMethod {
        TagMethod::ClassifierDev
    }
}

fn stub_classifiers(tags: &[&str], confidence: Confidence) -> Classifiers {
    Classifiers::new(
        Stub::answering(tags, confidence),
        Stub::answering(tags, confidence),
        test_canon(),
    )
}

/// Primary always errors; the fallback is the real `Deterministic` impl over
/// the test vocabulary - today's Tier 1, run on the note's own tags.
fn failing_with_deterministic_fallback() -> Classifiers {
    Classifiers::new(
        Stub::failing(),
        Box::new(Deterministic::new(test_canon(), Default::default())),
        test_canon(),
    )
}

fn apply_opts() -> ClassifyOpts {
    ClassifyOpts {
        apply: true,
        path: None,
        force: false,
        review_only: false,
        retag: Vec::new(),
    }
}

#[test]
fn test_filter_inbox_notes() {
    let inbox_note = NoteBuilder::new("inbox/test.md").title("Test").build();
    let notes_note = NoteBuilder::new("notes/other.md").title("Other").build();
    let notes = vec![inbox_note, notes_note];

    let filtered = filter_inbox_notes(&notes, false, false);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].path.to_string_lossy(), "inbox/test.md");
}

#[test]
fn test_filter_skips_already_classified() {
    let mut note = NoteBuilder::new("inbox/test.md").title("Test").build();
    note.frontmatter
        .extra
        .insert("cortex-classified".to_string(), serde_yaml::Value::Bool(true));
    let notes = vec![note];

    let filtered = filter_inbox_notes(&notes, false, false);
    assert_eq!(filtered.len(), 0);

    // With force=true, should include it
    let filtered = filter_inbox_notes(&notes, true, false);
    assert_eq!(filtered.len(), 1);
}

#[test]
fn test_resolve_collision_no_conflict() {
    let path = PathBuf::from("/tmp/nonexistent-classify-test-12345.md");
    assert_eq!(resolve_collision(&path, None), path);
}

/// Phase 5 fix, `2026-08-15-harvest-note-identity-trace-keyed-replace.md`
/// prior attempt 4: a base-path collision with a DIFFERENT source correctly
/// mints `-2`, but the bug walked past every SAME-source numeric candidate
/// (`-5`, `-7` .. `-14` in the real `hv-e5d240` cohort) because
/// `existing_note_has_source` was only ever applied to the base path. This
/// asserts the repaired loop overwrites the first same-source `-N` candidate
/// it finds instead of minting `-N+1`.
#[test]
fn test_resolve_collision_overwrites_same_source_numeric_candidate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().join("note.md");
    let source = "clyde://8d6b6ef3-aaaa-bbbb-cccc-dddddddddddd";
    // Genuinely different sources (not a suffixed variant of `source`, which
    // `existing_note_has_source`'s substring `contains` would still match).
    let other_source = "clyde://eb65b08e-1111-2222-3333-444444444444";

    // Base path collides with a DIFFERENT source.
    std::fs::write(&base, format!("---\nsource: {other_source}\n---\nbody\n")).expect("write base");
    // -2 and -3 are also different sources (real siblings from other sessions).
    std::fs::write(
        dir.path().join("note-2.md"),
        format!("---\nsource: {other_source}\n---\nbody\n"),
    )
    .expect("write -2");
    std::fs::write(
        dir.path().join("note-3.md"),
        format!("---\nsource: {other_source}\n---\nbody\n"),
    )
    .expect("write -3");
    // -4 carries the SAME source: this is the candidate that must be
    // overwritten rather than skipped in favor of -5.
    std::fs::write(
        dir.path().join("note-4.md"),
        format!("---\nsource: {source}\n---\nold body\n"),
    )
    .expect("write -4");

    let resolved = resolve_collision(&base, Some(source));
    assert_eq!(resolved, dir.path().join("note-4.md"));
    // -5 must never have been consulted/created by this call.
    assert!(!dir.path().join("note-5.md").exists());
}

/// Mirror-image control: when NO numeric candidate shares the source, the
/// loop still mints the first free `-N` slot exactly as before the fix.
#[test]
fn test_resolve_collision_mints_next_free_slot_when_no_candidate_matches_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dir.path().join("note.md");
    let source = "clyde://8d6b6ef3-aaaa-bbbb-cccc-dddddddddddd";
    let other_source = "clyde://eb65b08e-1111-2222-3333-444444444444";

    std::fs::write(&base, format!("---\nsource: {other_source}\n---\nbody\n")).expect("write base");
    std::fs::write(
        dir.path().join("note-2.md"),
        format!("---\nsource: {other_source}\n---\nbody\n"),
    )
    .expect("write -2");

    let resolved = resolve_collision(&base, Some(source));
    assert_eq!(resolved, dir.path().join("note-3.md"));
}

/// Enrichment writes the P3 union (preserved first, then fresh) and the three
/// provenance keys.
#[test]
fn test_build_enrichment_fields_unions_tags() {
    let note = NoteBuilder::new("inbox/test.md").title("Test").tags(&["rust"]).build();
    let result = ClassifyResult {
        tags: TagOutput {
            tags: vec!["ai".to_string(), "llm".to_string()],
            scores: None,
            confidence: Confidence::High,
            method: TagMethod::ClassifierDev,
        },
        reason: "test".to_string(),
    };

    let fields = build_enrichment_fields(&result, &note, &test_canon());
    let tags = fields
        .iter()
        .find(|(k, _)| k == "tags")
        .map(|(_, v)| v.clone())
        .expect("tags field");
    assert_eq!(
        tags,
        tags_value(&["rust".to_string(), "ai".to_string(), "llm".to_string()])
    );
    assert!(
        fields
            .iter()
            .any(|(k, v)| k == "status" && v == &serde_yaml::Value::String("unread".to_string()))
    );
    assert!(
        fields
            .iter()
            .any(|(k, v)| k == "cortex-classified" && v == &serde_yaml::Value::Bool(true))
    );
    assert!(
        fields
            .iter()
            .any(|(k, v)| k == "cortex-classified-by" && v == &serde_yaml::Value::String("classifier-dev".to_string()))
    );
}

/// The cap belongs to the preserved set: a note already at it keeps its own
/// tags and the fresh ones are dropped, the same policy a reingest applies.
#[test]
fn test_build_enrichment_fields_cap_keeps_preserved() {
    let note = NoteBuilder::new("inbox/test.md")
        .title("Test")
        .tags(&["rust", "cli"])
        .build();
    let result = ClassifyResult {
        tags: TagOutput {
            tags: vec!["ai".to_string()],
            scores: None,
            confidence: Confidence::High,
            method: TagMethod::ClassifierDev,
        },
        reason: "test".to_string(),
    };

    let fields = build_enrichment_fields(
        &result,
        &note,
        &CanonicalSet {
            max_per_note: 2,
            ..test_canon()
        },
    );
    let tags = fields.iter().find(|(k, _)| k == "tags").map(|(_, v)| v.clone());
    assert_eq!(
        tags,
        Some(tags_value(&["rust".to_string(), "cli".to_string()])),
        "the fresh tag must lose to the cap, not the preserved ones"
    );
}

#[test]
fn test_filter_unclassified_notes_selects_tagless_in_notes() {
    let tagless = NoteBuilder::new("notes/orphaned.md").title("Orphaned").build();
    let classified = NoteBuilder::new("notes/good.md").title("Good").tags(&["tech"]).build();
    let inbox = NoteBuilder::new("inbox/new.md").title("New").build();
    let notes = vec![tagless, classified, inbox];

    let filtered = filter_unclassified_notes(&notes, &FrontmatterConfig::default());
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].path.to_string_lossy(), "notes/orphaned.md");
}

#[test]
fn test_filter_unclassified_notes_ignores_inbox() {
    let inbox_no_tags = NoteBuilder::new("inbox/test.md").title("Test").build();
    let notes = vec![inbox_no_tags];

    let filtered = filter_unclassified_notes(&notes, &FrontmatterConfig::default());
    assert_eq!(filtered.len(), 0);
}

#[test]
fn test_catchup_classify_enriches_in_place() {
    use crate::testutil::TestVault;

    let vault = TestVault::new();
    // Add a tag-less note in notes/ (orphaned by reingest) for catch-up to enrich
    vault.add_note(
            "notes/reingest-orphan.md",
            "---\ntitle: Reingest Orphan\ndate: 2026-03-20\ntype: link\ntags: []\nsource: \"https://example.com/rust-guide\"\n---\nA reingested note that lost its tags.\n",
        );

    let notes = vault.scan();
    let config = vault.config();
    let (report, written) = apply_classify(
        vault.root(),
        &notes,
        &config,
        &apply_opts(),
        &stub_classifiers(&["rust", "cli"], Confidence::High),
    )
    .unwrap();

    // The orphan was actually written, so it is in the written-paths list.
    assert!(
        written.iter().any(|p| p.contains("reingest-orphan")),
        "catch-up write should be surfaced in written_paths: {written:?}"
    );

    // The orphan should have been catch-up classified
    let content = vault.read("notes/reingest-orphan.md");
    assert!(content.contains("- rust"), "should have fresh tags assigned");
    assert!(
        content.contains("cortex-classified: true"),
        "should be marked classified"
    );

    // Report should mention catch-up
    let violations: Vec<_> = report
        .violations
        .iter()
        .filter(|v| v.path.to_string_lossy().contains("reingest-orphan"))
        .collect();
    assert!(!violations.is_empty(), "should have a violation for the orphan");
    assert!(
        violations[0].message.contains("catch-up"),
        "violation message should mention catch-up"
    );
}

#[test]
fn test_lint_classify_includes_unclassified_notes() {
    let inbox_note = NoteBuilder::new("inbox/test.md").title("Test").tags(&["rust"]).build();
    let orphan = NoteBuilder::new("notes/orphan.md").title("Orphan").build();
    let notes = vec![inbox_note, orphan];

    let config = Config::default();
    let opts = ClassifyOpts {
        apply: false,
        ..apply_opts()
    };
    let report = lint_classify(
        &notes,
        Path::new("/nonexistent-vault-root"),
        &config,
        &opts,
        &stub_classifiers(&["rust"], Confidence::High),
    );
    // Both should produce violations
    let paths: Vec<String> = report
        .violations
        .iter()
        .map(|v| v.path.to_string_lossy().to_string())
        .collect();
    assert!(paths.iter().any(|p| p.contains("inbox/test.md")));
    assert!(paths.iter().any(|p| p.contains("notes/orphan.md")));
}

// ---- catch-up scope: path-exempt notes are not "unclassified" ----

#[test]
fn unclassified_filter_skips_tags_exempt_paths() {
    use std::collections::HashMap;

    let vault = crate::testutil::TestVault::new();
    vault.add_note(
        "notes/ai/daily/2026-07-10.md",
        "---\ntitle: Daily\ndate: 2026-07-10\ntype: daily\norigin: generated\ntags: []\n---\n\nbody\n",
    );
    vault.add_note(
        "notes/genuinely-unclassified.md",
        "---\ntitle: Orphan\ndate: 2026-07-10\ntype: note\norigin: assisted\ntags: []\n---\n\nbody\n",
    );
    let notes = vault.scan();

    // With no exemptions configured, both are candidates.
    let bare = FrontmatterConfig::default();
    let picked = filter_unclassified_notes(&notes, &bare);
    assert_eq!(
        picked.len(),
        2,
        "got {:?}",
        picked.iter().map(|n| &n.path).collect::<Vec<_>>()
    );

    // With the live config's `notes/ai/** -> [tags]` exemption, the digest is
    // correctly tag-less and must NOT be re-classified every cycle.
    let mut path_exempt = HashMap::new();
    path_exempt.insert(
        "notes/ai/**".to_string(),
        vec!["tags".to_string(), "origin".to_string()],
    );
    let exempting = FrontmatterConfig {
        path_exempt,
        ..FrontmatterConfig::default()
    };
    let picked = filter_unclassified_notes(&notes, &exempting);
    assert_eq!(picked.len(), 1);
    assert!(picked[0].path.ends_with("genuinely-unclassified.md"));
}

// ---- Phase 7: promotion gates on the classifier's confidence ----

/// Build a one-note inbox vault carrying `tags`, run one apply, and report
/// `(promoted, held)` where `held` means the note stayed in `inbox/` with
/// `cortex-needs-review: true`.
fn promote_once(classifiers: &Classifiers, tags: &str) -> (bool, bool, String) {
    let vault = crate::testutil::TestVault::new();
    vault.add_note(
        "inbox/candidate.md",
        &format!("---\ntitle: Candidate\ndate: 2026-09-20\ntype: link\n{tags}source: \"https://example.com/candidate\"\n---\n\n## Summary\n\nA note waiting on classification.\n"),
    );
    let notes = vault.scan();
    let config = vault.config();
    let (_report, _written) = apply_classify(vault.root(), &notes, &config, &apply_opts(), classifiers).expect("apply");

    let promoted = vault.root().join("notes/candidate.md").exists();
    let inbox_path = vault.root().join("inbox/candidate.md");
    let held = inbox_path.exists()
        && std::fs::read_to_string(&inbox_path)
            .expect("read held note")
            .contains("cortex-needs-review: true");
    let content = if promoted {
        vault.read("notes/candidate.md")
    } else {
        vault.read("inbox/candidate.md")
    };
    (promoted, held, content)
}

/// The confidence table drives promotion: High and Medium promote, Low holds.
#[test]
fn promotion_gates_on_confidence() {
    let (promoted, held, content) = promote_once(&stub_classifiers(&["rust", "cli"], Confidence::High), "");
    assert!(promoted, "High must promote");
    assert!(!held);
    assert!(content.contains("cortex-confidence: high"), "content was:\n{content}");
    assert!(
        content.contains("- rust"),
        "the classified tags are written:\n{content}"
    );

    let (promoted, held, content) = promote_once(&stub_classifiers(&["rust"], Confidence::Medium), "");
    assert!(promoted, "Medium must promote");
    assert!(!held);
    assert!(content.contains("cortex-confidence: medium"), "content was:\n{content}");

    let (promoted, held, content) = promote_once(&stub_classifiers(&[], Confidence::Low), "");
    assert!(!promoted, "Low must NOT promote");
    assert!(held, "Low holds the note for review, content was:\n{content}");
    assert!(
        !content.contains("cortex-classified: true"),
        "a held note is never marked classified:\n{content}"
    );
}

/// A classifier outage must not stall a note borg already tagged at ingest:
/// the configured `deterministic` fallback re-derives the same tags locally
/// from the note's own canonical tags, and says so in `cortex-classified-by`.
#[test]
fn deterministic_fallback_preserves_tier_one() {
    let (promoted, held, content) = promote_once(&failing_with_deterministic_fallback(), "tags:\n  - rust\n  - cli\n");
    assert!(promoted, "the fallback's answer must promote the note:\n{content}");
    assert!(!held);
    assert!(
        content.contains("cortex-classified-by: deterministic"),
        "the fallback's method is what gets recorded:\n{content}"
    );
    assert!(
        content.contains("- rust") && content.contains("- cli"),
        "content was:\n{content}"
    );
}

/// `--retag` is replace, not union - with two guards. A protected tag
/// (`no-classifier-tags`) survives, an unprotected one does not, and a note is
/// never left with an empty tag list.
#[test]
fn retag_replaces_and_never_writes_empty() {
    let vault = crate::testutil::TestVault::new();
    vault.add_note(
        "notes/keeper.md",
        "---\ntitle: Keeper\ndate: 2026-09-20\ntype: note\ntags:\n  - rust\n  - work\n---\n\nbody\n",
    );
    let opts = ClassifyOpts {
        retag: vec!["notes/keeper.md".to_string()],
        ..apply_opts()
    };
    let config = vault.config();

    let protecting = protecting_canon(&["work"]);
    let classifiers = Classifiers::new(
        Stub::answering(&["llm"], Confidence::High),
        Stub::answering(&["llm"], Confidence::High),
        protecting.clone(),
    );
    let (_report, written) = apply_classify(vault.root(), &vault.scan(), &config, &opts, &classifiers).expect("retag");
    assert_eq!(written, vec!["notes/keeper.md".to_string()]);

    let content = vault.read("notes/keeper.md");
    assert!(content.contains("- work"), "the protected tag survives:\n{content}");
    assert!(content.contains("- llm"), "the fresh tag is written:\n{content}");
    assert!(
        !content.contains("- rust"),
        "an unprotected tag is REPLACED, not unioned:\n{content}"
    );

    // Second pass: the classifier comes back empty. The note must keep what it
    // has rather than being left with no tags at all.
    let empty = Classifiers::new(
        Stub::answering(&[], Confidence::Low),
        Stub::answering(&[], Confidence::Low),
        protecting,
    );
    let (_report, written) = apply_classify(vault.root(), &vault.scan(), &config, &opts, &empty).expect("retag");
    assert!(written.is_empty(), "an empty result writes nothing: {written:?}");
    let after = vault.read("notes/keeper.md");
    assert!(
        after.contains("- work") && after.contains("- llm"),
        "content was:\n{after}"
    );
}

/// An absolute `--retag` argument selects the note whose vault-relative path
/// is its tail - anchored on the separator. `notes/home.md` must not also
/// select the vault-root `home.md`, which is a different note.
#[test]
fn retag_absolute_paths_match_on_a_path_boundary() {
    let root_home = NoteBuilder::new("home.md").title("Vault Index").build();
    let notes_home = NoteBuilder::new("notes/home.md").title("Home").build();
    let other = NoteBuilder::new("notes/other.md").title("Other").build();
    let notes = vec![root_home, notes_home, other];

    let root = Path::new("/home/saidler/repos/scottidler/obsidian");
    let selected = filter_retag_notes(
        &notes,
        &["/home/saidler/repos/scottidler/obsidian/notes/home.md".to_string()],
        root,
    );
    assert_eq!(
        selected.len(),
        1,
        "got {:?}",
        selected.iter().map(|n| &n.path).collect::<Vec<_>>()
    );
    assert_eq!(selected[0].path.to_string_lossy(), "notes/home.md");

    // A vault-relative argument and a glob both still work, and neither
    // selects a note twice.
    let selected = filter_retag_notes(&notes, &["notes/*.md".to_string(), "notes/home.md".to_string()], root);
    assert_eq!(selected.len(), 2);
}

/// Implementation audit r1, M1: the reproduced sequence, end to end.
///
/// On the panel's fixture `sb cortex classify --retag --apply` wrote nine tags
/// against a cap of eight, and the next `sb cortex sweep --migrate` re-capped
/// the note without a protect list and dropped `work`. The cortex daemon runs
/// sweep on a cadence, so no operator had to ask for that second step. Both
/// halves are fixed here: retag respects the cap, sweep respects the protect
/// list, and the protected tag survives both.
#[test]
fn retag_then_sweep_keeps_the_protected_tag() {
    // `sweep::migrate` calls `validate_canonical_assets`, which resolves the
    // real XDG_CONFIG_HOME - see the lock comment in `sweep/tests.rs`.
    let _lock = crate::testutil::lock_env();
    let _cfg = crate::testutil::hermetic_config_home();

    let vault = crate::testutil::TestVault::new();
    vault.add_note(
        "notes/keeper.md",
        "---\ntitle: Keeper\ndate: 2026-09-20\ntype: note\ntags:\n  - work\n  - rust\n---\n\nbody\n",
    );
    let opts = ClassifyOpts {
        retag: vec!["notes/keeper.md".to_string()],
        ..apply_opts()
    };
    let config = vault.config();

    // Cap 3, `work` protected, and a fresh result that already fills the cap:
    // the pre-fix code wrote four tags here.
    let canon = CanonicalSet {
        no_classifier: ["work".to_string()].into_iter().collect(),
        max_per_note: 3,
        ..test_canon()
    };
    let classifiers = Classifiers::new(
        Stub::answering(&["ai", "cli", "llm"], Confidence::High),
        Stub::answering(&["ai", "cli", "llm"], Confidence::High),
        canon,
    );
    let (_report, written) = apply_classify(vault.root(), &vault.scan(), &config, &opts, &classifiers).expect("retag");
    assert_eq!(written, vec!["notes/keeper.md".to_string()]);

    let after_retag = vault.read("notes/keeper.md");
    let tag_lines = |content: &str| -> Vec<String> {
        content
            .lines()
            .skip_while(|l| !l.starts_with("tags:"))
            .skip(1)
            .take_while(|l| l.starts_with("  - "))
            .map(|l| l.trim_start_matches("  - ").to_string())
            .collect()
    };
    let retagged = tag_lines(&after_retag);
    assert_eq!(retagged.len(), 3, "retag wrote over the cap:\n{after_retag}");
    assert!(
        retagged.contains(&"work".to_string()),
        "the protected tag did not survive retag:\n{after_retag}"
    );

    // The daemon's next sweep, over the same vocabulary and protect list.
    let assets = tempfile::tempdir().expect("assets tmpdir");
    let canonical_path = assets.path().join("canonical-tags.yml");
    let mapping_path = assets.path().join("tag-mapping.yml");
    let proposals_path = assets.path().join("tag-proposals.yml");
    std::fs::write(
        &canonical_path,
        "max-per-note: 3\nmax-canonical: 300\nno-classifier-tags:\n  - work\ntags:\n  general:\n    - rust\n    - cli\n    - ai\n    - llm\n    - work\n    - programming\n",
    )
    .expect("write canonical");
    std::fs::write(&mapping_path, "{}\n").expect("write mapping");
    std::fs::write(&proposals_path, "proposals: []\n").expect("write proposals");
    let sweep_config = crate::config::SweepConfig {
        canonical_path,
        mapping_path,
        proposals_path,
        sweep_interval: "1h".to_string(),
        proposal_threshold: 2,
        cold: crate::config::ColdConfig::default(),
    };
    crate::sweep::migrate(vault.root(), &vault.scan(), &sweep_config, false).expect("sweep --migrate");

    let after_sweep = vault.read("notes/keeper.md");
    let swept = tag_lines(&after_sweep);
    assert!(
        swept.contains(&"work".to_string()),
        "sweep dropped the protected tag the retag kept:\n{after_sweep}"
    );
    assert!(swept.len() <= 3, "sweep left the note over the cap:\n{after_sweep}");
}

/// B1: the enrichment cap protects `no-classifier-tags`. A note whose own tags
/// already exceed the cap keeps the protected one; a plain truncate dropped it
/// because it sat last in the preserved list.
#[test]
fn enrichment_cap_keeps_the_protected_tag_when_the_note_already_exceeds_it() {
    let canon = CanonicalSet {
        max_per_note: 2,
        ..protecting_canon(&["work"])
    };
    let note = NoteBuilder::new("inbox/test.md")
        .title("Test")
        .tags(&["rust", "cli", "work"])
        .build();
    let result = ClassifyResult {
        tags: TagOutput {
            tags: vec!["ai".to_string()],
            scores: None,
            confidence: Confidence::High,
            method: TagMethod::ClassifierDev,
        },
        reason: "test".to_string(),
    };

    let fields = build_enrichment_fields(&result, &note, &canon);
    let tags = fields.iter().find(|(k, _)| k == "tags").map(|(_, v)| v.clone());
    assert_eq!(
        tags,
        Some(tags_value(&["rust".to_string(), "work".to_string()])),
        "the protected tag must survive the cap"
    );
}

/// B2: a retag result is reported as retained-protected plus model-selected.
#[test]
fn retag_split_counts_protected_carryovers_apart_from_model_picks() {
    let canon = protecting_canon(&["work"]);
    let previous = vec!["work".to_string(), "rust".to_string()];
    let fresh = vec!["work".to_string(), "ai".to_string(), "llm".to_string()];
    assert_eq!(retag_split(&previous, &fresh, &canon), (1, 2));

    // A protected tag the model picked on its own is a model pick, not a carryover.
    let fresh_only = vec!["work".to_string(), "ai".to_string()];
    assert_eq!(retag_split(&["rust".to_string()], &fresh_only, &canon), (0, 2));
}

/// B2: the retag report line carries both counts.
#[test]
fn retag_report_line_carries_retained_and_selected_counts() {
    let vault = crate::testutil::TestVault::new();
    vault.add_note(
        "notes/split.md",
        "---\ntitle: Split\ndate: 2026-03-20\ntype: note\norigin: assisted\ntags:\n  - work\n  - rust\n---\nBody.\n",
    );
    let classifiers = Classifiers::new(
        Stub::answering(&["ai", "llm"], Confidence::High),
        Stub::answering(&["ai", "llm"], Confidence::High),
        protecting_canon(&["work"]),
    );
    let opts = ClassifyOpts {
        retag: vec!["notes/split.md".to_string()],
        ..apply_opts()
    };
    let (report, _) = apply_classify(vault.root(), &vault.scan(), &vault.config(), &opts, &classifiers).expect("apply");
    let line = &report.violations[0].message;
    assert!(
        line.contains("retained-protected=1") && line.contains("model-selected=2"),
        "missing split counts: {line}"
    );
}

/// B4: the `fabric-closed` kind is selected when configured and a runner is
/// present, for both the primary and the fallback slot.
#[test]
fn fabric_closed_is_selected_when_configured_with_a_runner() {
    struct FakeRunner;
    impl FabricRunner for FakeRunner {
        fn run(&self, _pattern: &str, _input: &str) -> Result<String> {
            Ok(r#"{"tags":["rust"]}"#.to_string())
        }
    }
    let runner: Arc<dyn FabricRunner> = Arc::new(FakeRunner);

    let cfg = TagsClassifierConfig {
        classifier: ClassifierKind::FabricClosed,
        fallback: ClassifierKind::Deterministic,
        ..TagsClassifierConfig::default()
    };
    let pair = Classifiers::from_parts(&cfg, test_canon(), &TagMapping::default(), Some(runner.clone()));
    assert_eq!(pair.primary.method(), TagMethod::FabricClosed);
    assert_eq!(pair.fallback.method(), TagMethod::Deterministic);

    let cfg = TagsClassifierConfig {
        classifier: ClassifierKind::Deterministic,
        fallback: ClassifierKind::FabricClosed,
        ..TagsClassifierConfig::default()
    };
    let pair = Classifiers::from_parts(&cfg, test_canon(), &TagMapping::default(), Some(runner));
    assert_eq!(pair.fallback.method(), TagMethod::FabricClosed);
}

/// B4: `load` only builds a Fabric runner when a slot asks for it, and it
/// carries the `fabric:` block's knobs when it does.
#[test]
fn fabric_runner_is_built_only_when_a_slot_is_fabric_closed() {
    let fabric = FabricConfig {
        binary: "fabric-test".to_string(),
        model: "test-model".to_string(),
        ..FabricConfig::default()
    };
    let none = fabric_runner_for(&TagsClassifierConfig::default(), &fabric);
    assert!(none.is_none(), "deterministic pair must not shell out to fabric");

    let cfg = TagsClassifierConfig {
        classifier: ClassifierKind::FabricClosed,
        ..TagsClassifierConfig::default()
    };
    let some = fabric_runner_for(&cfg, &fabric);
    assert!(some.is_some(), "fabric-closed needs a runner");
    let pair = Classifiers::from_parts(&cfg, test_canon(), &TagMapping::default(), some);
    assert_eq!(pair.primary.method(), TagMethod::FabricClosed);
}
