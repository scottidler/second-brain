use serde_yaml::Value;

use super::group_by_slug;
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
