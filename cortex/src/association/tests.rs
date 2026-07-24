use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eyre::Result;
use serde_yaml::Value;

use super::{
    AssociationOutcome, AssociationReport, AtomicWriter, DecideCtx, EmbeddingCosine, NoteWriter, append_bullets, apply,
    decide, execute_cross_link, execute_merge, group_by_slug,
};
use crate::config::{AssociationConfig, SimilaritySource};
use crate::testutil::NoteBuilder;
use crate::vault::{Note, parse_note};

fn session_note(path: &str, slug: &str) -> Note {
    NoteBuilder::new(path)
        .note_type("session")
        .extra("slug", Value::String(slug.to_string()))
        .build()
}

#[test]
fn groups_same_slug_session_notes_and_excludes_the_rest() {
    let notes = vec![
        // The one real group: two session notes sharing slug "foo".
        session_note("a.md", "foo"),
        session_note("b.md", "foo"),
        // Legacy pre-slug session note: slug == None, never groups.
        NoteBuilder::new("legacy.md").note_type("session").build(),
        // Tombstone: carries a matching slug AND superseded-by - already
        // absorbed, must never re-group even though the slug matches.
        NoteBuilder::new("tombstone.md")
            .note_type("session")
            .extra("slug", Value::String("foo".to_string()))
            .extra("superseded-by", Value::String("a".to_string()))
            .build(),
        // Non-session note sharing the same slug - out of scope entirely
        // (cross-slug/cross-type dedup is cortex::duplicates' job).
        NoteBuilder::new("article.md")
            .note_type("article")
            .extra("slug", Value::String("foo".to_string()))
            .build(),
    ];

    let groups = group_by_slug(&notes);

    assert_eq!(
        groups.len(),
        1,
        "only the foo-slug session pair forms a group: {groups:?}"
    );
    assert_eq!(groups[0], vec![0, 1]);
}

#[test]
fn drops_singleton_groups() {
    let notes = vec![session_note("solo.md", "unique-slug")];
    let groups = group_by_slug(&notes);
    assert!(groups.is_empty(), "a lone slug member has nothing to associate with");
}

#[test]
fn groups_are_btreemap_ordered_by_slug() {
    // Two distinct multi-member groups; the returned order must be
    // deterministic (sorted by slug), not scan/insertion order.
    let notes = vec![
        session_note("z1.md", "zzz"),
        session_note("z2.md", "zzz"),
        session_note("a1.md", "aaa"),
        session_note("a2.md", "aaa"),
    ];

    let groups = group_by_slug(&notes);

    assert_eq!(groups.len(), 2);
    // "aaa" sorts before "zzz", so its group (indices 2,3) comes first
    // regardless of the notes slice's scan order.
    assert_eq!(groups[0], vec![2, 3]);
    assert_eq!(groups[1], vec![0, 1]);
}

#[test]
fn empty_input_yields_no_groups() {
    let notes: Vec<Note> = Vec::new();
    assert!(group_by_slug(&notes).is_empty());
}

#[test]
fn promoted_sim_fns_are_callable_from_association() {
    // Phase 1 promotes duplicates::{tokenize, cosine_similarity} from
    // private `fn` to `pub(crate)` specifically so association's future
    // claim-text similarity fallback (Phase 2) can call them cross-module.
    // This test is the compile-time + behavioral proof that promotion
    // actually landed and the primitives still behave correctly.
    let a = crate::duplicates::tokenize("durable execution temporal workflow");
    let b = crate::duplicates::tokenize("durable execution temporal workflow");

    let a_tfidf: std::collections::HashMap<&str, f64> = a.iter().map(|(term, &count)| (*term, count as f64)).collect();
    let b_tfidf: std::collections::HashMap<&str, f64> = b.iter().map(|(term, &count)| (*term, count as f64)).collect();

    let identical = crate::duplicates::cosine_similarity(&a_tfidf, &b_tfidf);
    assert!(
        (identical - 1.0).abs() < 1e-9,
        "identical term vectors cosine to 1.0, got {identical}"
    );

    let empty: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
    let uncomputable = crate::duplicates::cosine_similarity(&a_tfidf, &empty);
    assert_eq!(uncomputable, 0.0, "an empty vector cosines to 0.0, never NaN/panic");
}

// -- Phase 2: decision core (transitive clustering) ------------------------

/// Deterministic embedding-cosine fake. Keyed on an order-independent pair of
/// paths; an unset pair returns `Ok(None)` (uncomputable via the embedding
/// signal), matching `SearchIndex::cosine_between`'s "either note lacks a
/// summary embedding" contract.
#[derive(Default)]
struct FakeEmbeddings {
    sims: HashMap<(PathBuf, PathBuf), Option<f32>>,
}

impl FakeEmbeddings {
    fn set(&mut self, a: &str, b: &str, sim: Option<f32>) {
        self.sims.insert(order(Path::new(a), Path::new(b)), sim);
    }
}

impl EmbeddingCosine for FakeEmbeddings {
    fn cosine_between(&self, a: &Path, b: &Path) -> Result<Option<f32>> {
        Ok(self.sims.get(&order(a, b)).copied().flatten())
    }
}

fn order(a: &Path, b: &Path) -> (PathBuf, PathBuf) {
    if a <= b {
        (a.to_path_buf(), b.to_path_buf())
    } else {
        (b.to_path_buf(), a.to_path_buf())
    }
}

/// A session note with a `date` and a single primary `cortex-session-ids`
/// entry (the survivor-selection inputs).
fn dated_session(path: &str, date: &str, id: &str) -> Note {
    NoteBuilder::new(path)
        .note_type("session")
        .date(date)
        .extra("slug", Value::String("foo".to_string()))
        .extra(
            "cortex-session-ids",
            Value::Sequence(vec![Value::String(id.to_string())]),
        )
        .build()
}

/// Same, plus a `## Claims` body section (the TF-IDF fallback input).
fn dated_session_with_claims(path: &str, date: &str, id: &str, claims: &str) -> Note {
    let mut note = dated_session(path, date, id);
    note.body = format!("## Summary\n\nx\n\n## Claims\n\n{claims}\n");
    note
}

fn ctx<'a>(embeddings: &'a FakeEmbeddings, threshold: f64, source: SimilaritySource) -> DecideCtx<'a, FakeEmbeddings> {
    DecideCtx {
        threshold,
        similarity_source: source,
        embeddings,
    }
}

fn refs(notes: &[Note]) -> Vec<&Note> {
    notes.iter().collect()
}

#[test]
fn pair_at_or_above_threshold_merges() {
    let notes = vec![
        dated_session("a.md", "2026-07-01", "aaa"),
        dated_session("b.md", "2026-07-10", "bbb"),
    ];
    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.90));

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();

    assert_eq!(
        outcomes,
        vec![AssociationOutcome::Merge {
            // Earliest date wins survivorship; b is absorbed.
            survivor: PathBuf::from("a.md"),
            absorbed: vec![PathBuf::from("b.md")],
            session_ids: vec!["aaa".to_string(), "bbb".to_string()],
        }],
        "a >=threshold pair merges into one survivor, no cross-link"
    );
}

#[test]
fn exactly_at_threshold_merges() {
    // Boundary: `>=` threshold merges. Break-the-code guard for a `>` typo.
    let notes = vec![
        dated_session("a.md", "2026-07-01", "aaa"),
        dated_session("b.md", "2026-07-02", "bbb"),
    ];
    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.85));

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();
    assert!(
        matches!(outcomes.as_slice(), [AssociationOutcome::Merge { .. }]),
        "similarity exactly at threshold merges (>=, not >): {outcomes:?}"
    );
}

#[test]
fn pair_below_threshold_cross_links() {
    // The negative of `pair_at_or_above_threshold_merges`: flip the pair below
    // threshold and it must CROSS-LINK, never merge (design Testing Strategy).
    let notes = vec![
        dated_session("a.md", "2026-07-01", "aaa"),
        dated_session("b.md", "2026-07-10", "bbb"),
    ];
    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.50));

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();

    assert_eq!(
        outcomes,
        vec![AssociationOutcome::CrossLink {
            notes: vec![PathBuf::from("a.md"), PathBuf::from("b.md")],
        }],
        "a <threshold pair cross-links and is NOT merged"
    );
    assert!(
        !outcomes.iter().any(|o| matches!(o, AssociationOutcome::Merge { .. })),
        "no merge on a below-threshold pair"
    );
}

#[test]
fn three_member_group_merges_close_pair_cross_links_distant_third() {
    // A~B >= threshold, but C is below threshold to BOTH: Merge{A,B} plus a
    // CrossLink joining the AB survivor and C (transitive clustering, not
    // star). This is the exact 3-member criterion from the design.
    let notes = vec![
        dated_session("a.md", "2026-07-01", "aaa"),
        dated_session("b.md", "2026-07-10", "bbb"),
        dated_session("c.md", "2026-07-05", "ccc"),
    ];
    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.92));
    embed.set("a.md", "c.md", Some(0.40));
    embed.set("b.md", "c.md", Some(0.30));

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();

    assert_eq!(
        outcomes,
        vec![
            AssociationOutcome::Merge {
                survivor: PathBuf::from("a.md"),
                absorbed: vec![PathBuf::from("b.md")],
                session_ids: vec!["aaa".to_string(), "bbb".to_string()],
            },
            AssociationOutcome::CrossLink {
                // representatives: AB's survivor (a.md) and the singleton c.md
                notes: vec![PathBuf::from("a.md"), PathBuf::from("c.md")],
            },
        ],
        "Merge{{A,B}} + CrossLink{{survivor, C}}"
    );
}

#[test]
fn transitive_chain_merges_all_three_even_when_ends_are_distant() {
    // A~B and B~C are both >= threshold, but A~C is BELOW threshold. A pure
    // star centered anywhere would drop a member; transitive union-find pulls
    // all three into one cluster. This is what "transitive, not star" means.
    let notes = vec![
        dated_session("a.md", "2026-07-01", "aaa"),
        dated_session("b.md", "2026-07-02", "bbb"),
        dated_session("c.md", "2026-07-03", "ccc"),
    ];
    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.90));
    embed.set("b.md", "c.md", Some(0.90));
    embed.set("a.md", "c.md", Some(0.10)); // ends are far apart

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();

    assert_eq!(
        outcomes,
        vec![AssociationOutcome::Merge {
            survivor: PathBuf::from("a.md"),
            absorbed: vec![PathBuf::from("b.md"), PathBuf::from("c.md")],
            session_ids: vec!["aaa".to_string(), "bbb".to_string(), "ccc".to_string()],
        }],
        "one transitive cluster absorbs all three, no leftover cross-link"
    );
}

#[test]
fn uncomputable_pair_cross_links_never_merges() {
    // No embedding set (fake returns Ok(None)) AND empty bodies (no claim
    // tokens) -> uncomputable -> below-threshold -> CrossLink.
    let notes = vec![
        dated_session("a.md", "2026-07-01", "aaa"),
        dated_session("b.md", "2026-07-10", "bbb"),
    ];
    let embed = FakeEmbeddings::default(); // no pair set

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();

    assert_eq!(
        outcomes,
        vec![AssociationOutcome::CrossLink {
            notes: vec![PathBuf::from("a.md"), PathBuf::from("b.md")],
        }],
        "an uncomputable pair cross-links"
    );
    assert!(
        !outcomes.iter().any(|o| matches!(o, AssociationOutcome::Merge { .. })),
        "uncomputable never merges"
    );
}

#[test]
fn uncomputable_never_merges_even_at_zero_threshold() {
    // The fail-safe's teeth: even with threshold 0.0 (where any COMPUTED
    // similarity would merge), an uncomputable pair must still cross-link. An
    // unknown is not a zero - it is never unioned.
    let notes = vec![
        dated_session("a.md", "2026-07-01", "aaa"),
        dated_session("b.md", "2026-07-10", "bbb"),
    ];
    let embed = FakeEmbeddings::default();

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.0, SimilaritySource::Both)).unwrap();

    assert!(
        matches!(outcomes.as_slice(), [AssociationOutcome::CrossLink { .. }]),
        "uncomputable stays cross-link even at threshold 0.0: {outcomes:?}"
    );
}

#[test]
fn claim_fallback_merges_unembedded_but_textually_identical_notes() {
    // No embedding on either side, but identical `## Claims` text -> the TF
    // fallback computes cosine 1.0 >= threshold -> Merge. Proves `Both` falls
    // through to claims when embeddings are absent.
    let claims = "- durable execution temporal workflow orchestration";
    let notes = vec![
        dated_session_with_claims("a.md", "2026-07-01", "aaa", claims),
        dated_session_with_claims("b.md", "2026-07-10", "bbb", claims),
    ];
    let embed = FakeEmbeddings::default(); // no embedding -> fallback

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();

    assert!(
        matches!(outcomes.as_slice(), [AssociationOutcome::Merge { .. }]),
        "identical claim text merges via the fallback: {outcomes:?}"
    );
}

#[test]
fn claim_fallback_cross_links_disjoint_claim_text() {
    // Both notes HAVE claims but share no terms -> a real Some(0.0)
    // measurement below threshold -> CrossLink (computed, not uncomputable).
    let notes = vec![
        dated_session_with_claims("a.md", "2026-07-01", "aaa", "- kubernetes networking ingress"),
        dated_session_with_claims("b.md", "2026-07-10", "bbb", "- rust ownership borrow checker"),
    ];
    let embed = FakeEmbeddings::default();

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();

    assert!(
        matches!(outcomes.as_slice(), [AssociationOutcome::CrossLink { .. }]),
        "disjoint claims cross-link: {outcomes:?}"
    );
}

#[test]
fn embedding_source_ignores_claim_text() {
    // Source = Embedding only. Identical claim text is present but the
    // embedding is absent -> uncomputable via the ONLY signal in play ->
    // CrossLink. Proves source selection actually gates the methodology.
    let claims = "- durable execution temporal workflow orchestration";
    let notes = vec![
        dated_session_with_claims("a.md", "2026-07-01", "aaa", claims),
        dated_session_with_claims("b.md", "2026-07-10", "bbb", claims),
    ];
    let embed = FakeEmbeddings::default();

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Embedding)).unwrap();

    assert!(
        matches!(outcomes.as_slice(), [AssociationOutcome::CrossLink { .. }]),
        "embedding-only source does not fall back to claims: {outcomes:?}"
    );
}

#[test]
fn claim_source_ignores_embedding() {
    // Source = Claim only. A high embedding cosine is present but claims are
    // empty -> uncomputable via the claim signal -> CrossLink. The mirror of
    // `embedding_source_ignores_claim_text`.
    let notes = vec![
        dated_session("a.md", "2026-07-01", "aaa"),
        dated_session("b.md", "2026-07-10", "bbb"),
    ];
    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.99));

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Claim)).unwrap();

    assert!(
        matches!(outcomes.as_slice(), [AssociationOutcome::CrossLink { .. }]),
        "claim-only source ignores a high embedding cosine: {outcomes:?}"
    );
}

#[test]
fn survivor_ties_broken_by_smallest_primary_session_id() {
    // Same date on both -> the smaller primary session id wins survivorship,
    // regardless of input order. `bbb` is listed first but `aaa` survives.
    let notes = vec![
        dated_session("first.md", "2026-07-05", "bbb"),
        dated_session("second.md", "2026-07-05", "aaa"),
    ];
    let mut embed = FakeEmbeddings::default();
    embed.set("first.md", "second.md", Some(0.95));

    let outcomes = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();

    assert_eq!(
        outcomes,
        vec![AssociationOutcome::Merge {
            survivor: PathBuf::from("second.md"), // id "aaa" < "bbb"
            absorbed: vec![PathBuf::from("first.md")],
            session_ids: vec!["aaa".to_string(), "bbb".to_string()],
        }],
        "equal dates: smallest primary id wins survivorship"
    );
}

#[test]
fn decide_is_deterministic_across_runs() {
    let notes = vec![
        dated_session("a.md", "2026-07-01", "aaa"),
        dated_session("b.md", "2026-07-02", "bbb"),
        dated_session("c.md", "2026-07-03", "ccc"),
        dated_session("d.md", "2026-07-04", "ddd"),
    ];
    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.90)); // cluster {a,b}
    embed.set("c.md", "d.md", Some(0.90)); // cluster {c,d}
    embed.set("a.md", "c.md", Some(0.10));

    let first = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();
    let second = decide(&refs(&notes), &ctx(&embed, 0.85, SimilaritySource::Both)).unwrap();

    assert_eq!(first, second, "decide is deterministic");
    assert_eq!(
        first,
        vec![
            AssociationOutcome::Merge {
                survivor: PathBuf::from("a.md"),
                absorbed: vec![PathBuf::from("b.md")],
                session_ids: vec!["aaa".to_string(), "bbb".to_string()],
            },
            AssociationOutcome::Merge {
                survivor: PathBuf::from("c.md"),
                absorbed: vec![PathBuf::from("d.md")],
                session_ids: vec!["ccc".to_string(), "ddd".to_string()],
            },
            AssociationOutcome::CrossLink {
                notes: vec![PathBuf::from("a.md"), PathBuf::from("c.md")],
            },
        ],
        "two merge clusters emit in ascending order, then one cross-link of both survivors"
    );
}

#[test]
fn embedding_db_error_propagates_not_swallowed() {
    // A genuine DB error from the port is propagated as Err, never silently
    // degraded to "uncomputable" (which would misroute every pair to
    // cross-link on a broken index).
    struct BrokenEmbeddings;
    impl EmbeddingCosine for BrokenEmbeddings {
        fn cosine_between(&self, _a: &Path, _b: &Path) -> Result<Option<f32>> {
            Err(eyre::eyre!("embedding index unavailable"))
        }
    }
    let notes = vec![
        dated_session("a.md", "2026-07-01", "aaa"),
        dated_session("b.md", "2026-07-10", "bbb"),
    ];
    let broken = BrokenEmbeddings;
    let ctx = DecideCtx {
        threshold: 0.85,
        similarity_source: SimilaritySource::Both,
        embeddings: &broken,
    };
    assert!(
        decide(&refs(&notes), &ctx).is_err(),
        "a real DB error surfaces, not swallowed as uncomputable"
    );
}

// -- Phase 3: merge executor -----------------------------------------------

/// Write a full harvest-shaped session note to `root/<name>` sharing slug
/// `foo`, with the given date, primary session id, `## Claims` bullets, and
/// `## Session Details` bullets. Mirrors what borg publishes so the executor's
/// section-union logic runs against realistic input.
fn write_session_file(root: &std::path::Path, name: &str, date: &str, id: &str, claims: &[&str], details: &[&str]) {
    let claims_block = claims.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n");
    let details_block = details.iter().map(|d| format!("- {d}")).collect::<Vec<_>>().join("\n");
    let content = format!(
        "---\n\
         title: {name}\n\
         date: {date}\n\
         type: session\n\
         slug: foo\n\
         cortex-session-ids:\n\
         - {id}\n\
         ---\n\
         ## Summary\n\n\
         summary of {name}\n\n\
         ## Claims\n\n\
         {claims_block}\n\n\
         ## Session Details\n\n\
         {details_block}\n"
    );
    std::fs::write(root.join(name), content).expect("write session file");
}

/// Parse every `.md` in `root` into `Note`s (path-sorted), the input to a
/// grouping+decide+execute run against the on-disk fixture.
fn scan_dir(root: &std::path::Path) -> Vec<Note> {
    let mut notes: Vec<Note> = std::fs::read_dir(root)
        .expect("read_dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .map(|p| parse_note(root, &p).expect("parse"))
        .collect();
    notes.sort_by(|a, b| a.path.cmp(&b.path));
    notes
}

/// One faithful association run over the on-disk vault: scan -> group -> decide
/// -> `execute_merge` per Merge outcome (the exact composition Phase 5's `apply`
/// will use). Returns the changed paths. Similarity is claim-based (empty
/// embeddings fall through to the TF fallback under `Both`).
fn associate_run<W: NoteWriter>(root: &std::path::Path, threshold: f64, writer: &W) -> Vec<String> {
    let embed = FakeEmbeddings::default();
    let notes = scan_dir(root);
    let groups = group_by_slug(&notes);
    let mut changed = Vec::new();
    for group in groups {
        let members: Vec<&Note> = group.iter().map(|&i| &notes[i]).collect();
        for outcome in decide(&members, &ctx(&embed, threshold, SimilaritySource::Both)).unwrap() {
            if let AssociationOutcome::Merge {
                survivor,
                absorbed,
                session_ids,
            } = outcome
            {
                changed.extend(execute_merge(root, &survivor, &absorbed, &session_ids, writer).unwrap());
            }
        }
    }
    changed
}

/// A writer that fails for exactly one absolute path (the mid-cluster
/// tombstone-write failure the self-heal test needs) and writes atomically
/// everywhere else.
struct FailWriter {
    fail: std::path::PathBuf,
}

impl NoteWriter for FailWriter {
    fn write(&self, dest: &std::path::Path, bytes: &[u8]) -> Result<()> {
        if dest == self.fail {
            return Err(eyre::eyre!("simulated write failure for {}", dest.display()));
        }
        vault::note::write_atomic(dest, bytes)
    }
}

#[test]
fn execute_merge_unions_claims_session_details_and_ids_and_tombstones_absorbed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_session_file(
        root,
        "a.md",
        "2026-07-01",
        "aaa",
        &["alpha beta gamma", "delta only-a"],
        &["clyde://aaa - A - `repo` - 5m"],
    );
    write_session_file(
        root,
        "b.md",
        "2026-07-10",
        "bbb",
        &["alpha beta gamma", "epsilon only-b"],
        &["clyde://bbb - B - `repo` - 3m"],
    );

    let changed = execute_merge(
        root,
        std::path::Path::new("a.md"),
        &[PathBuf::from("b.md")],
        &["aaa".to_string(), "bbb".to_string()],
        &AtomicWriter,
    )
    .unwrap();
    assert_eq!(changed, vec!["a.md".to_string(), "b.md".to_string()]);

    // Survivor carries the union of both id sets and both session-detail bullets,
    // plus the absorbed note's distinct claim.
    let survivor = parse_note(root, &root.join("a.md")).unwrap();
    let ids: Vec<String> = survivor
        .frontmatter
        .extra
        .get("cortex-session-ids")
        .and_then(|v| v.as_sequence())
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["aaa".to_string(), "bbb".to_string()], "union of both id sets");
    assert!(survivor.body.contains("clyde://aaa"), "keeps its own session detail");
    assert!(
        survivor.body.contains("clyde://bbb"),
        "gains the absorbed session detail"
    );
    assert!(survivor.body.contains("- delta only-a"), "keeps its own claim");
    assert!(survivor.body.contains("- epsilon only-b"), "gains the absorbed claim");

    // Absorbed note is a tombstone: superseded-by set, slug removed, redirect body.
    let tomb = parse_note(root, &root.join("b.md")).unwrap();
    assert_eq!(
        tomb.frontmatter.extra.get("superseded-by").and_then(|v| v.as_str()),
        Some("a"),
        "superseded-by points at the survivor stem"
    );
    assert!(
        !tomb.frontmatter.extra.contains_key("slug"),
        "slug removed so it never re-groups"
    );
    assert_eq!(tomb.body.trim(), "Merged into [[a]].", "body is a single redirect");
}

#[test]
fn second_run_is_byte_level_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_session_file(
        root,
        "a.md",
        "2026-07-01",
        "aaa",
        &["alpha beta gamma", "delta only-a"],
        &["clyde://aaa - A - `repo` - 5m"],
    );
    write_session_file(
        root,
        "b.md",
        "2026-07-10",
        "bbb",
        &["alpha beta gamma", "epsilon only-b"],
        &["clyde://bbb - B - `repo` - 3m"],
    );

    let first = associate_run(root, 0.5, &AtomicWriter);
    assert!(!first.is_empty(), "first run merges");
    let a_after1 = std::fs::read(root.join("a.md")).unwrap();
    let b_after1 = std::fs::read(root.join("b.md")).unwrap();

    // Second run: b is now a tombstone (slug removed), so the group collapses to
    // a singleton and decide is never even reached - zero writes.
    let second = associate_run(root, 0.5, &AtomicWriter);
    assert!(second.is_empty(), "second run writes nothing: {second:?}");
    assert_eq!(
        std::fs::read(root.join("a.md")).unwrap(),
        a_after1,
        "survivor byte-identical"
    );
    assert_eq!(
        std::fs::read(root.join("b.md")).unwrap(),
        b_after1,
        "tombstone byte-identical"
    );
}

#[test]
fn tombstone_write_failure_self_heals_next_run_with_no_duplication() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_session_file(
        root,
        "a.md",
        "2026-07-01",
        "aaa",
        &["alpha beta gamma", "delta only-a"],
        &["clyde://aaa - A - `repo` - 5m"],
    );
    write_session_file(
        root,
        "b.md",
        "2026-07-10",
        "bbb",
        &["alpha beta gamma", "epsilon only-b"],
        &["clyde://bbb - B - `repo` - 3m"],
    );

    // Run 1: the tombstone write for b FAILS mid-cluster. The survivor still gets
    // the full union; b is left un-retired (keeps its slug).
    let fail = FailWriter {
        fail: root.join("b.md"),
    };
    let changed1 = associate_run(root, 0.5, &fail);
    assert_eq!(changed1, vec!["a.md".to_string()], "only the survivor was written");
    let b1 = parse_note(root, &root.join("b.md")).unwrap();
    assert!(
        b1.frontmatter.extra.contains_key("slug"),
        "b keeps its slug after the failed retire"
    );
    assert!(
        !b1.frontmatter.extra.contains_key("superseded-by"),
        "b is not yet a tombstone"
    );

    // Run 2: b re-groups with the survivor and is re-absorbed cleanly.
    let changed2 = associate_run(root, 0.5, &AtomicWriter);
    assert_eq!(
        changed2,
        vec!["b.md".to_string()],
        "self-heal retires b; survivor already unioned, not rewritten"
    );

    // No duplication: each claim and each id appears exactly once in the survivor.
    let survivor = std::fs::read_to_string(root.join("a.md")).unwrap();
    assert_eq!(
        survivor.matches("- alpha beta gamma").count(),
        1,
        "shared claim not doubled"
    );
    assert_eq!(
        survivor.matches("- epsilon only-b").count(),
        1,
        "absorbed claim added once"
    );
    assert_eq!(
        survivor.matches("clyde://bbb").count(),
        1,
        "absorbed session detail added once"
    );
    let ids: Vec<String> = parse_note(root, &root.join("a.md"))
        .unwrap()
        .frontmatter
        .extra
        .get("cortex-session-ids")
        .and_then(|v| v.as_sequence())
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        vec!["aaa".to_string(), "bbb".to_string()],
        "ids deduped, no double-add"
    );

    // b is finally a tombstone.
    let b2 = parse_note(root, &root.join("b.md")).unwrap();
    assert_eq!(
        b2.frontmatter.extra.get("superseded-by").and_then(|v| v.as_str()),
        Some("a")
    );
    assert!(!b2.frontmatter.extra.contains_key("slug"));
}

#[test]
fn survivor_write_failure_skips_cluster_without_retiring_absorbed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_session_file(
        root,
        "a.md",
        "2026-07-01",
        "aaa",
        &["alpha beta gamma"],
        &["clyde://aaa - A - `repo` - 5m"],
    );
    write_session_file(
        root,
        "b.md",
        "2026-07-10",
        "bbb",
        &["epsilon only-b"],
        &["clyde://bbb - B - `repo` - 3m"],
    );
    let b_before = std::fs::read(root.join("b.md")).unwrap();

    // Survivor write fails -> whole cluster skipped, absorbed note untouched.
    let fail = FailWriter {
        fail: root.join("a.md"),
    };
    let changed = execute_merge(
        root,
        std::path::Path::new("a.md"),
        &[PathBuf::from("b.md")],
        &["aaa".to_string(), "bbb".to_string()],
        &fail,
    )
    .unwrap();
    assert!(changed.is_empty(), "no path reported when the survivor write fails");
    assert_eq!(
        std::fs::read(root.join("b.md")).unwrap(),
        b_before,
        "absorbed note is NOT retired when the survivor never landed"
    );
    let b = parse_note(root, &root.join("b.md")).unwrap();
    assert!(
        b.frontmatter.extra.contains_key("slug"),
        "b keeps its slug, re-groups next run"
    );
}

#[test]
fn merge_leaves_unrelated_vault_files_untouched() {
    // The executor scopes its writes to exactly the cluster files. Borg's
    // receipts DB lives outside the vault and is never opened here; this proves
    // the executor does not touch any file beyond the survivor + absorbed set.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_session_file(
        root,
        "a.md",
        "2026-07-01",
        "aaa",
        &["alpha beta gamma"],
        &["clyde://aaa - A - `repo` - 5m"],
    );
    write_session_file(
        root,
        "b.md",
        "2026-07-10",
        "bbb",
        &["epsilon only-b"],
        &["clyde://bbb - B - `repo` - 3m"],
    );
    let sentinel = root.join("receipts.db");
    std::fs::write(&sentinel, b"receipts-rows-unchanged").unwrap();

    execute_merge(
        root,
        std::path::Path::new("a.md"),
        &[PathBuf::from("b.md")],
        &["aaa".to_string(), "bbb".to_string()],
        &AtomicWriter,
    )
    .unwrap();

    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"receipts-rows-unchanged",
        "an unrelated file (stand-in for the receipts DB) is untouched"
    );
}

#[test]
fn append_bullets_is_idempotent_when_all_present() {
    let content = "---\ntitle: A\n---\n## Claims\n\n- one\n- two\n";
    let incoming = vec![vec!["- one".to_string()], vec!["- two".to_string()]];
    let out = append_bullets(content, "## Claims", &incoming, super::claim_key);
    assert_eq!(out, content, "re-adding present bullets is a byte-level no-op");
}

// -- Phase 4: cross-link executor -------------------------------------------

/// A minimal note (frontmatter + body) for cross-link fixtures - the executor
/// only touches the body's `## Related` section, so these don't need the full
/// harvest-session shape `write_session_file` builds.
fn write_plain_note(root: &std::path::Path, name: &str, body: &str) {
    let content = format!("---\ntitle: {name}\n---\n{body}");
    std::fs::write(root.join(name), content).expect("write plain note");
}

#[test]
fn execute_cross_link_inserts_reciprocal_related_section() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_plain_note(root, "a.md", "## Summary\n\nfoo\n");
    write_plain_note(root, "b.md", "## Summary\n\nbar\n");

    let changed = execute_cross_link(root, &[PathBuf::from("a.md"), PathBuf::from("b.md")], &AtomicWriter).unwrap();
    assert_eq!(changed, vec!["a.md".to_string(), "b.md".to_string()]);

    let a = std::fs::read_to_string(root.join("a.md")).unwrap();
    let b = std::fs::read_to_string(root.join("b.md")).unwrap();
    assert!(a.contains("## Related"), "a gains a Related section: {a}");
    assert!(a.contains("- [[b]]"), "a links to b by its filename stem: {a}");
    assert!(b.contains("## Related"), "b gains a Related section: {b}");
    assert!(b.contains("- [[a]]"), "b links to a by its filename stem: {b}");
}

#[test]
fn execute_cross_link_appends_to_an_existing_related_section() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_plain_note(root, "a.md", "## Related\n\n- [[preexisting]]\n\n## Summary\n\nfoo\n");
    write_plain_note(root, "b.md", "## Summary\n\nbar\n");

    let changed = execute_cross_link(root, &[PathBuf::from("a.md"), PathBuf::from("b.md")], &AtomicWriter).unwrap();
    assert_eq!(changed, vec!["a.md".to_string(), "b.md".to_string()]);

    let a = std::fs::read_to_string(root.join("a.md")).unwrap();
    assert_eq!(
        a.matches("## Related").count(),
        1,
        "the existing section is reused, never duplicated: {a}"
    );
    assert!(
        a.contains("- [[preexisting]]"),
        "the pre-existing bullet is preserved: {a}"
    );
    assert!(a.contains("- [[b]]"), "the sibling link is appended: {a}");
}

#[test]
fn execute_cross_link_skips_a_link_already_present_in_piped_form() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // a already links to b, via a piped alias - `related_key` recognizes the
    // target before the `|`, so this counts as "already present".
    write_plain_note(root, "a.md", "## Related\n\n- [[b|Some Alias]]\n\n## Summary\n\nfoo\n");
    write_plain_note(root, "b.md", "## Summary\n\nbar\n");

    let changed = execute_cross_link(root, &[PathBuf::from("a.md"), PathBuf::from("b.md")], &AtomicWriter).unwrap();

    // a is unchanged (already linked); b still gains the reciprocal link.
    assert_eq!(
        changed,
        vec!["b.md".to_string()],
        "a untouched, only b's missing reciprocal link is written: {changed:?}"
    );
    let a = std::fs::read_to_string(root.join("a.md")).unwrap();
    assert_eq!(
        a.matches("[[b").count(),
        1,
        "the existing piped link is not duplicated: {a}"
    );
}

#[test]
fn execute_cross_link_three_members_each_gains_the_other_two() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_plain_note(root, "a.md", "## Summary\n\nfoo\n");
    write_plain_note(root, "b.md", "## Summary\n\nbar\n");
    write_plain_note(root, "c.md", "## Summary\n\nbaz\n");

    let changed = execute_cross_link(
        root,
        &[PathBuf::from("a.md"), PathBuf::from("b.md"), PathBuf::from("c.md")],
        &AtomicWriter,
    )
    .unwrap();
    assert_eq!(
        changed,
        vec!["a.md".to_string(), "b.md".to_string(), "c.md".to_string()]
    );

    let a = std::fs::read_to_string(root.join("a.md")).unwrap();
    let b = std::fs::read_to_string(root.join("b.md")).unwrap();
    let c = std::fs::read_to_string(root.join("c.md")).unwrap();
    assert!(
        a.contains("- [[b]]") && a.contains("- [[c]]"),
        "a links to b and c: {a}"
    );
    assert!(
        b.contains("- [[a]]") && b.contains("- [[c]]"),
        "b links to a and c: {b}"
    );
    assert!(
        c.contains("- [[a]]") && c.contains("- [[b]]"),
        "c links to a and b: {c}"
    );
}

#[test]
fn execute_cross_link_skips_an_unreadable_member_and_still_links_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_plain_note(root, "a.md", "## Summary\n\nfoo\n");
    write_plain_note(root, "b.md", "## Summary\n\nbar\n");
    // "missing.md" is named in the outcome but does not exist on disk (e.g. a
    // stale run against a note deleted out-of-band): it must never appear as
    // a link target and must never be written to.
    let changed = execute_cross_link(
        root,
        &[
            PathBuf::from("a.md"),
            PathBuf::from("b.md"),
            PathBuf::from("missing.md"),
        ],
        &AtomicWriter,
    )
    .unwrap();
    assert_eq!(changed, vec!["a.md".to_string(), "b.md".to_string()]);
    let a = std::fs::read_to_string(root.join("a.md")).unwrap();
    let b = std::fs::read_to_string(root.join("b.md")).unwrap();
    assert!(
        !a.contains("missing"),
        "the unreadable member never becomes a link target: {a}"
    );
    assert!(
        !b.contains("missing"),
        "the unreadable member never becomes a link target: {b}"
    );
    assert!(!root.join("missing.md").exists(), "never created");
}

#[test]
fn execute_cross_link_with_fewer_than_two_readable_members_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_plain_note(root, "a.md", "## Summary\n\nfoo\n");
    let changed = execute_cross_link(
        root,
        &[PathBuf::from("a.md"), PathBuf::from("missing.md")],
        &AtomicWriter,
    )
    .unwrap();
    assert!(
        changed.is_empty(),
        "a lone readable member has no sibling to link: {changed:?}"
    );
    let a = std::fs::read_to_string(root.join("a.md")).unwrap();
    assert!(
        !a.contains("## Related"),
        "no Related section is created for nothing to link: {a}"
    );
}

#[test]
fn second_cross_link_run_writes_zero_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_plain_note(root, "a.md", "## Summary\n\nfoo\n");
    write_plain_note(root, "b.md", "## Summary\n\nbar\n");
    let members = [PathBuf::from("a.md"), PathBuf::from("b.md")];

    let first = execute_cross_link(root, &members, &AtomicWriter).unwrap();
    assert!(!first.is_empty(), "first run writes the reciprocal links");
    let a_after1 = std::fs::read(root.join("a.md")).unwrap();
    let b_after1 = std::fs::read(root.join("b.md")).unwrap();

    let second = execute_cross_link(root, &members, &AtomicWriter).unwrap();
    assert!(second.is_empty(), "second run writes zero bytes: {second:?}");
    assert_eq!(std::fs::read(root.join("a.md")).unwrap(), a_after1, "a byte-identical");
    assert_eq!(std::fs::read(root.join("b.md")).unwrap(), b_after1, "b byte-identical");
}

#[test]
fn related_key_extracts_target_before_pipe_case_insensitively() {
    assert_eq!(super::related_key("- [[Foo]]"), "foo");
    assert_eq!(super::related_key("- [[foo|Foo Title]]"), "foo");
    assert_eq!(super::related_key("* [[bar|Alias]]"), "bar");
}

// -- Phase 5: apply orchestrator (CLI + daemon wiring) ----------------------

/// `min_quiescence_secs: 0` never treats a just-written test fixture (mtime
/// ~now) as quiescing: `elapsed < Duration::ZERO` is never true. Used by every
/// test below that wants the quiescence guard to be a no-op so it can assert
/// on grouping/decide/execute behavior in isolation.
fn no_quiescence_config(threshold: f64) -> AssociationConfig {
    AssociationConfig {
        threshold,
        min_quiescence_secs: 0,
        ..AssociationConfig::default()
    }
}

#[test]
fn dry_run_reports_the_plan_and_writes_zero_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_session_file(
        root,
        "a.md",
        "2026-07-01",
        "aaa",
        &["alpha beta gamma"],
        &["clyde://aaa - A - `repo` - 5m"],
    );
    write_session_file(
        root,
        "b.md",
        "2026-07-10",
        "bbb",
        &["alpha beta gamma"],
        &["clyde://bbb - B - `repo` - 3m"],
    );
    let a_before = std::fs::read(root.join("a.md")).unwrap();
    let b_before = std::fs::read(root.join("b.md")).unwrap();

    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.95));
    let notes = scan_dir(root);

    let report = apply(root, &notes, &no_quiescence_config(0.85), &embed, false).unwrap();

    assert!(
        matches!(report, AssociationReport::WouldAssociate(_)),
        "no --apply -> WouldAssociate: {report:?}"
    );
    assert_eq!(report.outcomes().len(), 1, "the plan names the one merge cluster");
    assert!(
        matches!(report.outcomes()[0], AssociationOutcome::Merge { .. }),
        "the planned outcome is the merge decide would produce: {:?}",
        report.outcomes()[0]
    );
    assert!(!report.applied(), "WouldAssociate.applied() is false");

    assert_eq!(std::fs::read(root.join("a.md")).unwrap(), a_before, "a untouched");
    assert_eq!(std::fs::read(root.join("b.md")).unwrap(), b_before, "b untouched");
}

#[test]
fn apply_executes_the_plan_and_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_session_file(
        root,
        "a.md",
        "2026-07-01",
        "aaa",
        &["alpha beta gamma"],
        &["clyde://aaa - A - `repo` - 5m"],
    );
    write_session_file(
        root,
        "b.md",
        "2026-07-10",
        "bbb",
        &["alpha beta gamma"],
        &["clyde://bbb - B - `repo` - 3m"],
    );

    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.95));
    let notes = scan_dir(root);

    let report = apply(root, &notes, &no_quiescence_config(0.85), &embed, true).unwrap();

    assert!(report.applied(), "--apply -> Associated");
    assert_eq!(report.outcomes().len(), 1);
    let survivor = parse_note(root, &root.join("a.md")).unwrap();
    assert!(survivor.body.contains("clyde://bbb"), "survivor absorbed b's detail");
    let tomb = parse_note(root, &root.join("b.md")).unwrap();
    assert_eq!(
        tomb.frontmatter.extra.get("superseded-by").and_then(|v| v.as_str()),
        Some("a"),
        "b is soft-retired"
    );

    // Re-running is a no-op: b dropped its slug, so the group no longer forms
    // and there is nothing left to decide, let alone associate.
    let notes2 = scan_dir(root);
    let report2 = apply(root, &notes2, &no_quiescence_config(0.85), &embed, true).unwrap();
    assert!(
        report2.outcomes().is_empty(),
        "idempotent re-run: {:?}",
        report2.outcomes()
    );
}

#[test]
fn whole_group_is_skipped_when_any_member_is_within_quiescence_window() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_session_file(
        root,
        "a.md",
        "2026-07-01",
        "aaa",
        &["alpha beta gamma"],
        &["clyde://aaa - A - `repo` - 5m"],
    );
    write_session_file(
        root,
        "b.md",
        "2026-07-10",
        "bbb",
        &["alpha beta gamma"],
        &["clyde://bbb - B - `repo` - 3m"],
    );
    let a_before = std::fs::read(root.join("a.md")).unwrap();
    let b_before = std::fs::read(root.join("b.md")).unwrap();

    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.95));
    let notes = scan_dir(root);

    // An absurdly large window means "just written" (mtime ~now) always
    // counts as quiescing - the whole group must be skipped even though the
    // pair would otherwise merge at this threshold.
    let quiescing = AssociationConfig {
        threshold: 0.85,
        min_quiescence_secs: 999_999,
        ..AssociationConfig::default()
    };

    let report = apply(root, &notes, &quiescing, &embed, true).unwrap();

    assert!(
        report.outcomes().is_empty(),
        "whole group skipped, no outcomes at all: {:?}",
        report.outcomes()
    );
    assert_eq!(std::fs::read(root.join("a.md")).unwrap(), a_before, "a untouched");
    assert_eq!(std::fs::read(root.join("b.md")).unwrap(), b_before, "b untouched");
}

#[test]
fn quiescence_skip_is_whole_group_never_half_merged() {
    // Three same-slug members where the pairwise similarities would otherwise
    // cluster {a,b} and cross-link {c}; quiescence must drop the ENTIRE group,
    // not just the one member technically within the window.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_session_file(root, "a.md", "2026-07-01", "aaa", &["x"], &["clyde://aaa"]);
    write_session_file(root, "b.md", "2026-07-02", "bbb", &["x"], &["clyde://bbb"]);
    write_session_file(root, "c.md", "2026-07-03", "ccc", &["y"], &["clyde://ccc"]);

    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "b.md", Some(0.95));
    embed.set("a.md", "c.md", Some(0.10));
    embed.set("b.md", "c.md", Some(0.10));
    let notes = scan_dir(root);

    let quiescing = AssociationConfig {
        threshold: 0.85,
        min_quiescence_secs: 999_999,
        ..AssociationConfig::default()
    };

    let report = apply(root, &notes, &quiescing, &embed, true).unwrap();
    assert!(
        report.outcomes().is_empty(),
        "one recently-modified member (all of them, in this fixture) skips the WHOLE 3-member group: {:?}",
        report.outcomes()
    );
    for name in ["a.md", "b.md", "c.md"] {
        let note = parse_note(root, &root.join(name)).unwrap();
        assert!(
            !note.frontmatter.extra.contains_key("superseded-by"),
            "{name} was not merged"
        );
    }
}

#[test]
fn excluded_path_never_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("journal")).unwrap();
    write_session_file(root, "a.md", "2026-07-01", "aaa", &["x"], &["clyde://aaa"]);
    write_session_file(
        &root.join("journal"),
        "b.md",
        "2026-07-10",
        "bbb",
        &["x"],
        &["clyde://bbb"],
    );

    let mut embed = FakeEmbeddings::default();
    embed.set("a.md", "journal/b.md", Some(0.99));
    let notes = scan_dir_recursive(root);

    let excluded = AssociationConfig {
        threshold: 0.85,
        min_quiescence_secs: 0,
        exclude: vec!["journal/**".to_string()],
        ..AssociationConfig::default()
    };

    let report = apply(root, &notes, &excluded, &embed, false).unwrap();
    assert!(
        report.outcomes().is_empty(),
        "b.md is excluded, so a.md's slug group never reaches two members: {:?}",
        report.outcomes()
    );
}

/// Like `scan_dir`, but walks one level of subdirectories too (the exclude
/// test needs a `journal/` note, and its vault-relative path must include the
/// subdirectory for the `journal/**` glob to match).
fn scan_dir_recursive(root: &std::path::Path) -> Vec<Note> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<Note>) {
        for entry in std::fs::read_dir(dir).expect("read_dir").filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if path.extension().and_then(|x| x.to_str()) == Some("md") {
                out.push(parse_note(root, &path).expect("parse"));
            }
        }
    }
    let mut notes = Vec::new();
    walk(root, root, &mut notes);
    notes.sort_by(|a, b| a.path.cmp(&b.path));
    notes
}
