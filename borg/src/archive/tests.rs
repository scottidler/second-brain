use super::*;
use tempfile::TempDir;

fn distilled_with_transcript(transcript: Option<&str>) -> Distilled {
    Distilled {
        summary: "A summary.".to_string(),
        transcript: transcript.map(str::to_string),
        ..Distilled::default()
    }
}

#[test]
fn write_lands_distilled_yml_and_reads_back() {
    let root = TempDir::new().expect("tmp");
    let distilled = distilled_with_transcript(Some("the verbatim video transcript"));
    let dir = SourceArchive {
        root: root.path(),
        trace_id: "yt-1",
        distilled: &distilled,
        source_md: None,
    }
    .write()
    .expect("write");
    assert_eq!(dir, root.path().join("yt-1"));
    let yaml = std::fs::read_to_string(dir.join(DISTILLED_FILENAME)).expect("read");
    let back: Distilled = serde_yaml::from_str(&yaml).expect("parse");
    assert_eq!(back.transcript.as_deref(), Some("the verbatim video transcript"));
    assert!(
        !dir.join(SOURCE_FILENAME).exists(),
        "video/article write no transcript.md"
    );
    assert!(!dir.join("distilled.yml.tmp").exists(), "temp file must not linger");
    assert!(is_durable(root.path(), "yt-1"));
}

#[test]
fn write_lands_source_md_for_repo_and_thread_kinds() {
    let root = TempDir::new().expect("tmp");
    let distilled = distilled_with_transcript(None);
    let dir = SourceArchive {
        root: root.path(),
        trace_id: "gh-1",
        distilled: &distilled,
        source_md: Some("# owner/repo\n\nREADME body"),
    }
    .write()
    .expect("write");
    assert_eq!(
        std::fs::read_to_string(dir.join(SOURCE_FILENAME)).expect("read"),
        "# owner/repo\n\nREADME body"
    );
    assert!(dir.join(DISTILLED_FILENAME).is_file());
}

#[test]
fn write_under_an_unwritable_root_is_an_io_error() {
    // A root that is a regular file fails `create_dir_all` with ENOTDIR even
    // as root; a 0o555 dir does not, because root bypasses permission bits.
    let parent = TempDir::new().expect("tmp");
    let root = parent.path().join("sources");
    std::fs::write(&root, b"not a dir").expect("write file");
    let distilled = distilled_with_transcript(Some("t"));
    let err = SourceArchive {
        root: &root,
        trace_id: "yt-ro",
        distilled: &distilled,
        source_md: None,
    }
    .write()
    .expect_err("an unwritable root must fail the write");
    assert!(matches!(err, ArchiveError::Io { .. }), "got {err:?}");
    assert!(!is_durable(&root, "yt-ro"));
}

#[test]
fn trace_dir_rejects_anything_but_one_plain_component() {
    let root = Path::new("/archive");
    assert_eq!(trace_dir(root, "tg-abc").expect("ok"), root.join("tg-abc"));
    for bad in ["", ".", "..", "../etc", "a/b", "/abs"] {
        assert!(
            matches!(trace_dir(root, bad), Err(ArchiveError::InvalidTraceId(_))),
            "{bad:?} must be rejected"
        );
    }
}

#[test]
fn is_durable_needs_distilled_yml_not_just_the_dir() {
    let root = TempDir::new().expect("tmp");
    assert!(!is_durable(root.path(), "missing"));
    std::fs::create_dir(root.path().join("half")).expect("mkdir");
    std::fs::write(root.path().join("half").join(SOURCE_FILENAME), "x").expect("write");
    assert!(
        !is_durable(root.path(), "half"),
        "transcript.md alone is not a complete archive"
    );
    assert!(!is_durable(root.path(), ".."));
}
