//! Integration tests for the Phase 6 backfill sweep. Each test builds a
//! throwaway git-backed vault fixture, runs the sweep against it, and asserts
//! on-disk file contents afterward -- this is a destructive one-shot tool, so
//! the only trustworthy assertion is "what actually landed on disk".

use std::fs;
use std::path::Path;
use std::process::Command;

use strip_transcripts::{Disposition, ensure_clean_worktree, run};

const APRIL_NOTE: &str = "---\n\
title: Top 10 Claude Code Skills\n\
type: youtube\n\
ingested: 2026-04-15T00:00:00-07:00\n\
---\n\
\n\
> [!tldr]\n\
> April baseline note, never touched by this sweep.\n\
\n\
## Summary\n\
\n\
Some prose about the video.\n\
\n\
## Enumerated Points\n\
\n\
1. **First** [00:01:00]\n\
\n\
## Transcript\n\
\n\
WEBVTT\n\
00:00:00.000 --> 00:00:02.000\n\
Legacy body content that must survive untouched.\n";

const FRESH_VIDEO_NOTE: &str = "---\n\
title: New Fat Video\n\
type: youtube\n\
ingested: 2026-07-01T00:00:00-07:00\n\
---\n\
\n\
## Summary\n\
\n\
Fresh summary.\n\
\n\
## Links\n\
\n\
- https://example.com\n\
\n\
## Transcript\n\
\n\
Multibyte check: caf\u{e9}, na\u{ef}ve, \u{65e5}\u{672c}\u{8a9e}.\n\
\n\
Line two of the transcript body that should be gone after the sweep runs.\n";

const NO_INGESTED_NOTE: &str = "---\n\
title: Missing Ingested\n\
type: article\n\
---\n\
\n\
## Summary\n\
\n\
No ingested key at all.\n\
\n\
## Transcript\n\
\n\
Should be refused, not stripped.\n";

const NON_VIDEO_NOTE: &str = "---\n\
title: Just a note\n\
type: note\n\
ingested: 2026-07-01T00:00:00-07:00\n\
---\n\
\n\
## Transcript\n\
\n\
This kind is out of scope entirely: ignored, not even refused.\n";

fn write_note(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create note parent dir");
    }
    fs::write(path, content).expect("write fixture note");
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

fn init_clean_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "test"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "seed fixture"]);
}

#[test]
fn strips_post_cutoff_video_and_leaves_other_sections_byte_identical() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_note(dir.path(), "notes/fresh.md", FRESH_VIDEO_NOTE);
    init_clean_repo(dir.path());

    ensure_clean_worktree(dir.path()).expect("freshly committed repo is clean");
    let report = run(dir.path()).expect("run");

    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(report.stripped(), 1);
    assert_eq!(report.refused(), 0);

    let after = fs::read_to_string(dir.path().join("notes/fresh.md")).expect("read stripped note");
    assert!(!after.contains("## Transcript"));
    assert!(!after.contains("caf\u{e9}"));

    let expected_prefix = &FRESH_VIDEO_NOTE[..FRESH_VIDEO_NOTE.find("## Transcript").expect("fixture has heading")];
    assert_eq!(
        after, expected_prefix,
        "every section before ## Transcript must survive byte-identical"
    );
}

#[test]
fn pre_cutoff_april_note_is_out_of_scope_and_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_note(dir.path(), "notes/april.md", APRIL_NOTE);
    init_clean_repo(dir.path());

    let report = run(dir.path()).expect("run");
    assert_eq!(report.outcomes.len(), 1);
    assert_eq!(report.stripped(), 0);
    match &report.outcomes[0].disposition {
        Disposition::Refused(reason) => assert!(reason.contains("pre-cutoff"), "reason was: {reason}"),
        Disposition::Stripped => panic!("April baseline must never be stripped"),
    }

    let after = fs::read_to_string(dir.path().join("notes/april.md")).expect("read april note");
    assert_eq!(
        after, APRIL_NOTE,
        "April baseline must be byte-identical after the sweep"
    );
}

#[test]
fn missing_ingested_is_refused_not_stripped() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_note(dir.path(), "notes/no-ingested.md", NO_INGESTED_NOTE);
    init_clean_repo(dir.path());

    let report = run(dir.path()).expect("run");
    assert_eq!(report.stripped(), 0);
    match &report.outcomes[0].disposition {
        Disposition::Refused(reason) => assert!(reason.contains("missing ingested"), "reason was: {reason}"),
        Disposition::Stripped => panic!("a note with no ingested key must never be stripped"),
    }

    let after = fs::read_to_string(dir.path().join("notes/no-ingested.md")).expect("read note");
    assert_eq!(after, NO_INGESTED_NOTE);
}

#[test]
fn non_video_article_kinds_are_ignored_entirely() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_note(dir.path(), "notes/note.md", NON_VIDEO_NOTE);
    init_clean_repo(dir.path());

    let report = run(dir.path()).expect("run");
    assert!(
        report.outcomes.is_empty(),
        "a `note` kind must never appear in the manifest"
    );

    let after = fs::read_to_string(dir.path().join("notes/note.md")).expect("read note");
    assert_eq!(after, NON_VIDEO_NOTE);
}

#[test]
fn refuses_on_dirty_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_note(dir.path(), "notes/fresh.md", FRESH_VIDEO_NOTE);
    init_clean_repo(dir.path());

    // Dirty it after the initial commit.
    write_note(
        dir.path(),
        "notes/fresh.md",
        &format!("{FRESH_VIDEO_NOTE}\nextra uncommitted line\n"),
    );

    let err = ensure_clean_worktree(dir.path()).expect_err("dirty worktree must refuse");
    assert!(err.to_string().contains("dirty"), "error was: {err}");
}
