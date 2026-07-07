#![allow(clippy::unwrap_used)]

use super::*;
use vault::frontmatter::Frontmatter;

fn note_with(note_type: &str, ingested: Option<&str>, raw: &str) -> Note {
    let frontmatter = Frontmatter {
        note_type: Some(note_type.to_string()),
        ingested: ingested.map(str::to_string),
        ..Default::default()
    };
    Note {
        path: PathBuf::from("notes/test.md"),
        frontmatter,
        body: raw.to_string(),
        raw: raw.to_string(),
    }
}

#[test]
fn find_transcript_start_matches_exact_heading_line() {
    let text = "## Summary\n\nprose\n\n## Transcript\n\nverbatim\n";
    let idx = find_transcript_start(text).expect("heading present");
    assert_eq!(&text[idx..], "## Transcript\n\nverbatim\n");
}

#[test]
fn find_transcript_start_ignores_similar_but_different_headings() {
    // "## Transcripts" (plural) and "### Transcript" (demoted, as a backfilled
    // legacy body can legitimately contain) must NOT match the exact L2 heading.
    let text = "## Transcripts\n\nnot the real heading\n\n### Transcript\n\nnot L2 either\n";
    assert_eq!(find_transcript_start(text), None);
}

#[test]
fn find_transcript_start_returns_none_when_absent() {
    let text = "## Summary\n\nno transcript here\n";
    assert_eq!(find_transcript_start(text), None);
}

#[test]
fn in_scope_kind_covers_youtube_video_article_only() {
    assert!(in_scope_kind(&note_with("youtube", None, "")));
    assert!(in_scope_kind(&note_with("video", None, "")));
    assert!(in_scope_kind(&note_with("article", None, "")));
    assert!(!in_scope_kind(&note_with("note", None, "")));
    assert!(!in_scope_kind(&note_with("github", None, "")));
}

#[test]
fn in_scope_kind_false_when_note_type_missing_or_unknown() {
    let mut missing = note_with("youtube", None, "");
    missing.frontmatter.note_type = None;
    assert!(!in_scope_kind(&missing));

    let mut unknown = note_with("youtube", None, "");
    unknown.frontmatter.note_type = Some("not-a-real-kind".to_string());
    assert!(!in_scope_kind(&unknown));
}

#[test]
fn classify_refuses_missing_ingested() {
    let n = note_with("video", None, "## Transcript\n\nx\n");
    assert_eq!(
        classify(&n, cutoff()),
        Disposition::Refused("missing ingested".to_string())
    );
}

#[test]
fn classify_refuses_unparsable_ingested() {
    let n = note_with("video", Some("not-a-date"), "## Transcript\n\nx\n");
    match classify(&n, cutoff()) {
        Disposition::Refused(reason) => assert!(reason.contains("unparsable"), "reason was: {reason}"),
        Disposition::Stripped => panic!("must not strip a note with an unparsable ingested value"),
    }
}

#[test]
fn classify_refuses_pre_cutoff() {
    let n = note_with(
        "article",
        Some("2026-04-15T00:00:00-07:00"),
        "## Transcript\n\nlegacy body\n",
    );
    match classify(&n, cutoff()) {
        Disposition::Refused(reason) => assert!(reason.contains("pre-cutoff"), "reason was: {reason}"),
        Disposition::Stripped => panic!("pre-cutoff notes are protected legacy bodies"),
    }
}

#[test]
fn classify_refuses_when_no_transcript_section_present() {
    let n = note_with(
        "article",
        Some("2026-07-01T00:00:00Z"),
        "## Summary\n\nnothing to strip\n",
    );
    match classify(&n, cutoff()) {
        Disposition::Refused(reason) => assert!(reason.contains("no ## Transcript"), "reason was: {reason}"),
        Disposition::Stripped => panic!("nothing to strip means refused, not stripped"),
    }
}

#[test]
fn classify_strips_post_cutoff_with_transcript() {
    let n = note_with(
        "youtube",
        Some("2026-07-01T00:00:00Z"),
        "## Summary\n\nprose\n\n## Transcript\n\nverbatim\n",
    );
    assert_eq!(classify(&n, cutoff()), Disposition::Stripped);
}

#[test]
fn cutoff_is_the_documented_instant() {
    let expected = DateTime::parse_from_rfc3339("2026-06-28T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(cutoff(), expected);
}
