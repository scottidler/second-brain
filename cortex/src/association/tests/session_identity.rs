use super::*;

// -- Phase 5: group_by_session_identity --------------------------------
// docs/design/2026-08-15-harvest-note-identity-trace-keyed-replace.md

/// Build one note of the real `hv-e5d240` cohort: 15 notes, one `trace:`, one
/// `source:`, IDENTICAL `cortex-session-ids`, but 6 distinct `slug:` values
/// (plus one legacy note with no `slug:` at all) - exactly why slug-grouping
/// saw one group of 9 and dropped the other 6 (design doc "Evidence" section).
/// `slug` is `None` for the one note the doc records as having no `slug:` key.
fn hv_e5d240_note(path: &str, slug: Option<&str>) -> Note {
    let mut builder = NoteBuilder::new(path)
        .note_type("session")
        .trace("hv-e5d240")
        .source("clyde://8d6b6ef3-0000-0000-0000-000000000000")
        .extra(
            "cortex-session-ids",
            Value::Sequence(vec![Value::String("8d6b6ef3-0000-0000-0000-000000000000".to_string())]),
        );
    if let Some(slug) = slug {
        builder = builder.extra("slug", Value::String(slug.to_string()));
    }
    builder.build()
}

/// One of the five same-title notes from an OTHER session (design doc:
/// `hv-353663`/`hv-95813b`/`hv-efc530`/`hv-067d05`/`hv-e5c476`, each a
/// DIFFERENT primary session and a DIFFERENT trace, that only collided with
/// the `hv-e5d240` cohort because of the shared generic title-fallback slug -
/// the mirror-image case `group_by_slug` was blind to).
fn other_session_note(path: &str, trace: &str, primary_id: &str) -> Note {
    NoteBuilder::new(path)
        .note_type("session")
        .trace(trace)
        .source(&format!("clyde://{primary_id}"))
        .extra("slug", Value::String("review-ci-workflow-security-changes".to_string()))
        .extra(
            "cortex-session-ids",
            Value::Sequence(vec![Value::String(primary_id.to_string())]),
        )
        .build()
}

#[test]
fn groups_the_real_hv_e5d240_cohort_and_excludes_other_sessions_sharing_the_title_slug() {
    let mut notes = vec![
        hv_e5d240_note(
            "ci-yml-public-repo-reusable-workflow-migration.md",
            Some("ci-yml-public-repo-reusable-workflow-migration"),
        ),
        hv_e5d240_note(
            "ci-yml-public-reusable-workflow-migration.md",
            Some("ci-yml-public-reusable-workflow-migration"),
        ),
        hv_e5d240_note(
            "clyde-ci-public-reusable-workflow-migration.md",
            Some("clyde-ci-public-reusable-workflow-migration"),
        ),
        hv_e5d240_note(
            "clyde-ci-public-reusable-workflow-migration-review.md",
            Some("clyde-ci-public-reusable-workflow-migration-review"),
        ),
        hv_e5d240_note(
            "clyde-ci-yml-public-reusable-workflow-migration.md",
            Some("clyde-ci-yml-public-reusable-workflow-migration"),
        ),
        // The 9 sharing the generic title-fallback slug, plus -5 with no slug
        // at all.
        hv_e5d240_note("review-ci-workflow-security-changes-5.md", None),
    ];
    for i in 7..=15 {
        notes.push(hv_e5d240_note(
            &format!("review-ci-workflow-security-changes-{i}.md"),
            Some("review-ci-workflow-security-changes"),
        ));
    }
    assert_eq!(notes.len(), 15, "the real hv-e5d240 cohort is exactly 15 notes");

    // The five same-title notes from OTHER sessions - each a different trace
    // and a different primary session id.
    notes.push(other_session_note(
        "review-ci-workflow-security-changes.md",
        "hv-353663",
        "eb65b08e",
    ));
    notes.push(other_session_note(
        "review-ci-workflow-security-changes-2.md",
        "hv-95813b",
        "1a31236d",
    ));
    notes.push(other_session_note(
        "review-ci-workflow-security-changes-3.md",
        "hv-efc530",
        "ee0b75a3",
    ));
    notes.push(other_session_note(
        "review-ci-workflow-security-changes-4.md",
        "hv-067d05",
        "bc5c376c",
    ));
    notes.push(other_session_note(
        "review-ci-workflow-security-changes-6.md",
        "hv-e5c476",
        "7eff9ae9",
    ));

    let groups = group_by_session_identity(&notes);

    assert_eq!(
        groups.len(),
        1,
        "exactly one group: the hv-e5d240 cohort. The 5 other-session notes must not join it: {groups:?}"
    );
    let mut group = groups[0].clone();
    group.sort_unstable();
    assert_eq!(
        group,
        (0..15).collect::<Vec<usize>>(),
        "all 15 hv-e5d240 notes group together despite 6 distinct slugs plus one missing slug"
    );
}

#[test]
fn different_traces_sharing_a_primary_session_id_do_not_group() {
    // Same primary session id in cortex-session-ids, but different trace:
    // the follow-up case. Must never group under the trace-keyed track.
    let shared_id = Value::String("8d6b6ef3-shared".to_string());
    let a = NoteBuilder::new("a.md")
        .note_type("session")
        .trace("hv-aaaaaa")
        .extra("cortex-session-ids", Value::Sequence(vec![shared_id.clone()]))
        .build();
    let b = NoteBuilder::new("b.md")
        .note_type("session")
        .trace("hv-bbbbbb")
        .extra("cortex-session-ids", Value::Sequence(vec![shared_id]))
        .build();
    let notes = vec![a, b];

    assert!(
        group_by_session_identity(&notes).is_empty(),
        "different non-empty traces must never group, even sharing a primary session id"
    );
}

#[test]
fn a_superseded_note_is_never_a_group_member() {
    let a = NoteBuilder::new("a.md").note_type("session").trace("hv-cccccc").build();
    let b = NoteBuilder::new("b.md").note_type("session").trace("hv-cccccc").build();
    let tombstone = NoteBuilder::new("c.md")
        .note_type("session")
        .trace("hv-cccccc")
        .extra("superseded-by", Value::String("a".to_string()))
        .build();
    let notes = vec![a, b, tombstone];

    let groups = group_by_session_identity(&notes);
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0],
        vec![0, 1],
        "the superseded-by note (index 2) must never be a member"
    );
}

#[test]
fn legacy_notes_group_by_transitive_session_id_overlap_when_trace_is_absent() {
    // No trace: on ANY of these three notes - the legacy fallback track.
    // a~b share id1, b~c share id2: transitive closure must land all three
    // in one cluster even though a and c share no id directly.
    let a = NoteBuilder::new("a.md")
        .note_type("session")
        .extra(
            "cortex-session-ids",
            Value::Sequence(vec![Value::String("id1".to_string())]),
        )
        .build();
    let b = NoteBuilder::new("b.md")
        .note_type("session")
        .extra(
            "cortex-session-ids",
            Value::Sequence(vec![Value::String("id1".to_string()), Value::String("id2".to_string())]),
        )
        .build();
    let c = NoteBuilder::new("c.md")
        .note_type("session")
        .extra(
            "cortex-session-ids",
            Value::Sequence(vec![Value::String("id2".to_string())]),
        )
        .build();
    // An unrelated legacy note with no overlapping id must stay a singleton
    // and get dropped.
    let d = NoteBuilder::new("d.md")
        .note_type("session")
        .extra(
            "cortex-session-ids",
            Value::Sequence(vec![Value::String("id-unrelated".to_string())]),
        )
        .build();
    let notes = vec![a, b, c, d];

    let groups = group_by_session_identity(&notes);
    assert_eq!(groups.len(), 1);
    let mut group = groups[0].clone();
    group.sort_unstable();
    assert_eq!(group, vec![0, 1, 2]);
}
