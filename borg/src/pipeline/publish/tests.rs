use super::*;

#[test]
fn archive_write_into_read_only_dir_fails_publish_and_writes_no_note() {
    use std::os::unix::fs::PermissionsExt;
    let vault = tempfile::TempDir::new().expect("vault");
    let inbox = vault.path().join("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox");
    let archive_parent = tempfile::TempDir::new().expect("archive");
    let archive_root = archive_parent.path().join("sources");
    std::fs::create_dir(&archive_root).expect("mkdir");
    std::fs::set_permissions(&archive_root, std::fs::Permissions::from_mode(0o555)).expect("chmod");

    let distilled = vault::distilled::Distilled {
        summary: "s".to_string(),
        transcript: Some("the verbatim source".to_string()),
        ..Default::default()
    };
    let archive = crate::archive::SourceArchive {
        root: &archive_root,
        trace_id: "tr-ro",
        distilled: &distilled,
        source_md: None,
    };
    let desired = inbox.join("note.md");
    let result = archive_then_publish(
        &archive,
        &desired,
        b"---\ntitle: t\n---\n",
        None,
        std::slice::from_ref(&inbox),
    );
    std::fs::set_permissions(&archive_root, std::fs::Permissions::from_mode(0o755)).expect("chmod back");

    let err = result.expect_err("a read-only archive root must fail the publish");
    assert!(matches!(err, PublishError::ArchiveWrite(_)), "got {err:?}");
    let ingest = publish_failure_result(&err, IngestMethod::Cli, "https://example.com/x", "tr-ro");
    assert_eq!(ingest.failure_stage, Some(FailureStage::PublishFailed));
    assert_eq!(terminal_failure_stage(&ingest).as_str(), "publish-failed");
    assert!(!desired.exists(), "no vault note may be written when the archive fails");
    assert_eq!(std::fs::read_dir(&inbox).expect("inbox").count(), 0);
}

#[test]
fn archive_then_publish_writes_archive_before_note() {
    let vault = tempfile::TempDir::new().expect("vault");
    let inbox = vault.path().join("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox");
    let archive_root = tempfile::TempDir::new().expect("archive");
    let distilled = vault::distilled::Distilled {
        summary: "s".to_string(),
        ..Default::default()
    };
    let archive = crate::archive::SourceArchive {
        root: archive_root.path(),
        trace_id: "tr-ok",
        distilled: &distilled,
        source_md: Some("thread markdown"),
    };
    let desired = inbox.join("note.md");
    let path = archive_then_publish(&archive, &desired, b"body", None, std::slice::from_ref(&inbox)).expect("publish");
    assert_eq!(path, desired);
    assert!(desired.is_file());
    assert!(crate::archive::is_durable(archive_root.path(), "tr-ok"));
    assert_eq!(
        std::fs::read_to_string(archive_root.path().join("tr-ok").join("transcript.md")).expect("read"),
        "thread markdown"
    );
}

/// The frontmatter block of a composed note, parsed as YAML. Panics when the
/// block is not valid YAML, which is the failure these tests guard.
fn parse_frontmatter(note: &str) -> serde_yaml::Mapping {
    let body = note.strip_prefix("---\n").expect("opening fence");
    let end = body.find("\n---").expect("closing fence");
    serde_yaml::from_str(&body[..end]).unwrap_or_else(|e| panic!("frontmatter is not YAML ({e}):\n{note}"))
}

/// Capture notes that a single-line preserve reader or a raw `format!`
/// writer would corrupt: multiline, YAML-significant characters, quotes,
/// an indented line, a blank line, and a lone `Source:` line.
const TRICKY_CAPTURE_NOTES: &[&str] = &[
    "why: this explains the\nrouting bug we hit\n\n  - and the fix",
    "line one\nline two: with a colon\n# not a comment",
    "\"quoted\" and 'single' and a trailing backslash \\",
    "plain one-liner",
    "unicode \u{2014} dash\nand a tab\there",
];

#[test]
fn capture_note_round_trips_through_reader_writer_and_yaml() {
    let dir = tempfile::tempdir().unwrap();
    for (i, capture) in TRICKY_CAPTURE_NOTES.iter().enumerate() {
        // The note on disk, written the way `render_note` writes it, with a
        // cortex key after it so the reader must stop at the entry's end.
        let old = format!(
            "---\ntitle: Old\ncapture-note: {}\nstatus: read\ntags:\n  - rust\n---\nBody.\n",
            markdown::yaml_scalar(capture)
        );
        let path = dir.path().join(format!("old-{i}.md"));
        std::fs::write(&path, &old).unwrap();

        let fields = read_preserved_fields(&path).expect("read");
        assert!(
            fields.contains(&(
                markdown::CAPTURE_NOTE_KEY.to_string(),
                FieldValue::Scalar(capture.to_string())
            )),
            "reader did not decode {capture:?}: {fields:?}"
        );
        assert!(
            fields.contains(&("status".to_string(), FieldValue::Scalar("read".to_string()))),
            "{fields:?}"
        );
        assert!(
            fields.contains(&("tags".to_string(), FieldValue::List(vec!["rust".to_string()]))),
            "{fields:?}"
        );

        // The `--force` reingest: the fresh render carries no capture note.
        let fresh = "---\ntitle: New\nstatus: inbox\ntags:\n  - rust\n---\nNew body.\n";
        let written = apply_preserved_fields(fresh, &fields, None);
        let fm = parse_frontmatter(&written);
        assert_eq!(
            fm.get(markdown::CAPTURE_NOTE_KEY).and_then(|v| v.as_str()),
            Some(*capture),
            "YAML decode differs for {capture:?}:\n{written}"
        );

        // A second reingest reads back the same value.
        let again = dir.path().join(format!("again-{i}.md"));
        std::fs::write(&again, &written).unwrap();
        let reread = read_preserved_fields(&again).expect("reread");
        assert!(
            reread.contains(&(
                markdown::CAPTURE_NOTE_KEY.to_string(),
                FieldValue::Scalar(capture.to_string())
            )),
            "second read differs for {capture:?}: {reread:?}"
        );
    }
}

#[test]
fn fresh_capture_note_wins_over_the_preserved_one() {
    let fresh = format!(
        "---\ntitle: New\ncapture-note: {}\n---\nBody.\n",
        markdown::yaml_scalar("new reason\nsecond line")
    );
    let preserved = vec![(
        markdown::CAPTURE_NOTE_KEY.to_string(),
        FieldValue::Scalar("old reason".to_string()),
    )];
    let written = apply_preserved_fields(&fresh, &preserved, None);
    let fm = parse_frontmatter(&written);
    assert_eq!(
        fm.get(markdown::CAPTURE_NOTE_KEY).and_then(|v| v.as_str()),
        Some("new reason\nsecond line")
    );
    assert_eq!(written.matches("capture-note:").count(), 1, "{written}");
}

#[test]
fn undecodable_capture_note_fails_the_preserve_read_closed() {
    let dir = tempfile::tempdir().unwrap();
    let note = dir.path().join("bad.md");
    std::fs::write(
        &note,
        "---\ntitle: T\ncapture-note: \"unterminated\nstatus: read\n---\nBody.\n",
    )
    .unwrap();
    let err = read_preserved_fields(&note).expect_err("a capture note that is not YAML must not read as absent");
    assert!(format!("{err:#}").contains("capture-note"), "{err:#}");
}

#[test]
fn borg_preserve_keys_are_not_cortex_keys() {
    for key in markdown::BORG_PRESERVE_KEYS {
        assert!(
            !CORTEX_PRESERVE_KEYS.contains(key),
            "{key} is borg-owned and must not be claimed by cortex"
        );
    }
}

#[test]
fn preserved_boilerplate_capture_note_is_dropped_and_intent_survives() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = "---\ntitle: New\nstatus: inbox\n---\nNew body.\n";
    let cases: &[(&str, Option<&str>)] = &[
        ("Source: XDA", None),
        (
            "Source: XDA\nworth a reread for the routing fix",
            Some("worth a reread for the routing fix"),
        ),
        (
            "why: the routing bug\n\n  and the fix",
            Some("why: the routing bug\n\n  and the fix"),
        ),
    ];
    for (i, (on_disk, expected)) in cases.iter().enumerate() {
        let path = dir.path().join(format!("old-{i}.md"));
        let old = format!(
            "---\ntitle: Old\ncapture-note: {}\n---\nBody.\n",
            markdown::yaml_scalar(on_disk)
        );
        std::fs::write(&path, old).unwrap();
        let fields = read_preserved_fields(&path).expect("read");
        let written = apply_preserved_fields(fresh, &fields, None);
        let fm = parse_frontmatter(&written);
        assert_eq!(
            fm.get(markdown::CAPTURE_NOTE_KEY).and_then(|v| v.as_str()),
            *expected,
            "reingest of {on_disk:?}:\n{written}"
        );
    }
}

/// Regression (implementation audit round 1): a `--force` reingest must
/// write preserved cortex scalars raw. Writing them through `yaml_scalar`
/// quoted `true` into `'true'` and `0.85` into `'0.85'`, so the search index
/// read the note as unclassified and cortex re-classified it.
#[test]
fn preserved_cortex_bool_and_float_scalars_survive_reingest_typed() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.md");
    std::fs::write(
        &old,
        "---\ntitle: Old\nstatus: read\ncortex-classified: true\ncortex-confidence: 0.85\n---\nBody.\n",
    )
    .unwrap();
    let fresh = "---\ntitle: New\nstatus: inbox\n---\nNew body.\n";

    let mut note = old;
    for pass in 1..=2 {
        let fields = read_preserved_fields(&note).expect("read");
        let written = apply_preserved_fields(fresh, &fields, None);
        let fm = parse_frontmatter(&written);
        assert_eq!(
            fm.get("cortex-classified"),
            Some(&serde_yaml::Value::Bool(true)),
            "pass {pass}: cortex-classified must stay a YAML bool:\n{written}"
        );
        assert_eq!(
            fm.get("cortex-confidence").and_then(serde_yaml::Value::as_f64),
            Some(0.85),
            "pass {pass}: cortex-confidence must stay a YAML float:\n{written}"
        );
        assert_eq!(fm.get("status").and_then(|v| v.as_str()), Some("read"), "{written}");
        note = dir.path().join(format!("pass-{pass}.md"));
        std::fs::write(&note, &written).unwrap();
    }
}
