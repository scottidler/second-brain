use super::*;
use crate::frontmatter::Frontmatter;
use std::path::PathBuf;

fn note(path: &str, trace: Option<&str>, superseded_by: Option<&str>) -> Note {
    let mut frontmatter = Frontmatter {
        trace: trace.map(str::to_string),
        ..Default::default()
    };
    if let Some(target) = superseded_by {
        frontmatter.extra.insert(
            SUPERSEDED_BY_KEY.to_string(),
            serde_yaml::Value::String(target.to_string()),
        );
    }
    Note {
        path: PathBuf::from(path),
        frontmatter,
        body: String::new(),
        raw: String::new(),
    }
}

#[test]
fn a_trace_claimed_by_one_note_resolves_to_it() {
    let notes = vec![note("notes/a.md", Some("hv-1"), None)];
    let index = NoteIndex::build(&notes);
    assert_eq!(
        index.resolve_trace("hv-1").expect("trace must resolve").path,
        PathBuf::from("notes/a.md")
    );
}

#[test]
fn an_unclaimed_trace_resolves_to_nothing() {
    let notes = vec![note("notes/a.md", Some("hv-1"), None)];
    let index = NoteIndex::build(&notes);
    assert!(index.resolve_trace("hv-missing").is_none());
}

/// Reingest leaves the OLD note carrying the same `trace:`, so one trace can
/// match several notes. The tombstone picks the survivor. Measured on the live
/// vault: 20 of 20 ambiguous groups have exactly one non-superseded survivor.
#[test]
fn superseded_candidates_converge_on_the_one_survivor() {
    let notes = vec![
        note("notes/old.md", Some("hv-1"), Some("new")),
        note("notes/new.md", Some("hv-1"), None),
    ];
    let index = NoteIndex::build(&notes);
    assert_eq!(
        index.resolve_trace("hv-1").expect("trace must resolve").path,
        PathBuf::from("notes/new.md")
    );
}

/// A `superseded-by` naming a stem that is not in the vault is a dangling
/// marker. Ignoring it keeps the only remaining candidate in the running
/// rather than retiring the note nothing replaced.
#[test]
fn a_dangling_superseded_marker_is_ignored() {
    let notes = vec![note("notes/old.md", Some("hv-1"), Some("never-existed"))];
    let index = NoteIndex::build(&notes);
    assert_eq!(
        index.resolve_trace("hv-1").expect("trace must resolve").path,
        PathBuf::from("notes/old.md")
    );
}

#[test]
fn two_survivors_fall_through_rather_than_picking_arbitrarily() {
    let notes = vec![
        note("notes/a.md", Some("hv-1"), None),
        note("notes/b.md", Some("hv-1"), None),
    ];
    let index = NoteIndex::build(&notes);
    assert!(index.resolve_trace("hv-1").is_none());
}

/// A cycle retires every candidate, leaving zero survivors. Falling through is
/// correct: picking one arbitrarily would make `sources` churn between ticks.
#[test]
fn a_superseded_cycle_falls_through() {
    let notes = vec![
        note("notes/a.md", Some("hv-1"), Some("b")),
        note("notes/b.md", Some("hv-1"), Some("a")),
    ];
    let index = NoteIndex::build(&notes);
    assert!(index.resolve_trace("hv-1").is_none());
}

/// Six candidates, five retired: the live shape this rule was written for.
#[test]
fn six_candidates_converge_on_the_single_survivor() {
    let notes = vec![
        note("notes/v1.md", Some("hv-1"), Some("v6")),
        note("notes/v2.md", Some("hv-1"), Some("v6")),
        note("notes/v3.md", Some("hv-1"), Some("v6")),
        note("notes/v4.md", Some("hv-1"), Some("v6")),
        note("notes/v5.md", Some("hv-1"), Some("v6")),
        note("notes/v6.md", Some("hv-1"), None),
    ];
    let index = NoteIndex::build(&notes);
    assert_eq!(
        index.resolve_trace("hv-1").expect("trace must resolve").path,
        PathBuf::from("notes/v6.md")
    );
}

#[test]
fn contains_path_is_vault_relative() {
    let notes = vec![note("notes/a.md", None, None)];
    let index = NoteIndex::build(&notes);
    assert!(index.contains_path(Path::new("notes/a.md")));
    assert!(!index.contains_path(Path::new("inbox/a.md")));
}

/// 38 stems collide vault-wide, so a basename match is only usable when it is
/// unique.
#[test]
fn a_colliding_stem_is_not_unique() {
    let notes = vec![note("notes/a.md", None, None), note("inbox/a.md", None, None)];
    let index = NoteIndex::build(&notes);
    assert!(index.unique_by_stem("a").is_none());

    let single = vec![note("notes/only.md", None, None)];
    let index = NoteIndex::build(&single);
    assert_eq!(
        index.unique_by_stem("only").expect("unique stem must resolve").path,
        PathBuf::from("notes/only.md")
    );
}
