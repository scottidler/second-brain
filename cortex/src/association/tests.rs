use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eyre::Result;
use serde_yaml::Value;

use super::{AssociationOutcome, DecideCtx, EmbeddingCosine, decide, group_by_slug};
use crate::config::SimilaritySource;
use crate::testutil::NoteBuilder;
use crate::vault::Note;

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
