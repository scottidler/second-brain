use super::*;

#[test]
fn archive_write_under_unwritable_root_fails_publish_and_writes_no_note() {
    let vault = tempfile::TempDir::new().expect("vault");
    let inbox = vault.path().join("inbox");
    std::fs::create_dir_all(&inbox).expect("inbox");
    let archive_parent = tempfile::TempDir::new().expect("archive");
    let archive_root = archive_parent.path().join("sources");
    // A file where the root dir should be: ENOTDIR holds even when CI runs as root.
    std::fs::write(&archive_root, b"not a dir").expect("write file");

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

    let err = result.expect_err("an unwritable archive root must fail the publish");
    assert!(matches!(err, PublishError::ArchiveWrite(_)), "got {err:?}");
    let ingest = url_publish_failure_result(
        &UrlPublishError::from(err),
        IngestMethod::Cli,
        "https://example.com/x",
        "tr-ro",
    );
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

#[test]
fn test_find_note_by_source_in_notes_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let notes = vault.join("notes");
    std::fs::create_dir_all(&notes).expect("mkdir");

    let note_content = "---\ntitle: Test\nsource: \"https://example.com/article\"\n---\nBody.\n";
    std::fs::write(notes.join("test-article.md"), note_content).expect("write");

    let found = find_note_by_source(vault, "https://example.com/article");
    assert!(found.is_some(), "should find note by source URL");
    assert_eq!(found.unwrap().file_name().unwrap().to_str().unwrap(), "test-article.md");
}

/// 2,073 vault notes store `source:` bare. A reingest that missed them published
/// `-2` copies beside the originals (2026-10-15).
#[test]
fn test_find_note_by_source_matches_bare_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let notes = vault.join("notes");
    std::fs::create_dir_all(&notes).expect("mkdir");
    std::fs::write(
        notes.join("shorts.md"),
        "---\ntitle: Shorts\nsource: https://www.youtube.com/shorts/RmmQRV3tdmY\n---\nBody.\n",
    )
    .expect("write");
    std::fs::write(
        notes.join("decoy.md"),
        "---\ntitle: Decoy\nsource: https://www.youtube.com/shorts/RmmQRV3tdmY-other\n---\nsource: https://example.com/body-only\n",
    )
    .expect("write");

    let found = find_note_by_source(vault, "https://www.youtube.com/shorts/RmmQRV3tdmY").expect("bare source found");
    assert_eq!(found.file_name().unwrap(), "shorts.md");
    assert!(
        find_note_by_source(vault, "https://example.com/body-only").is_none(),
        "a source: line in the body is not the note's source"
    );
}

#[test]
fn test_find_note_by_source_in_inbox() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let inbox = vault.join("inbox");
    std::fs::create_dir_all(&inbox).expect("mkdir");

    let note_content = "---\ntitle: Inbox Note\nsource: \"https://example.com/inbox-item\"\n---\nBody.\n";
    std::fs::write(inbox.join("inbox-item.md"), note_content).expect("write");

    let found = find_note_by_source(vault, "https://example.com/inbox-item");
    assert!(found.is_some(), "should find note in inbox/");
    assert!(found.unwrap().starts_with(&inbox), "found path should be in inbox/");
}

#[test]
fn test_find_note_by_source_not_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let notes = vault.join("notes");
    std::fs::create_dir_all(&notes).expect("mkdir");

    let note_content = "---\ntitle: Other\nsource: \"https://other.com\"\n---\nBody.\n";
    std::fs::write(notes.join("other.md"), note_content).expect("write");

    let found = find_note_by_source(vault, "https://example.com/missing");
    assert!(found.is_none(), "should not find non-matching source");
}

#[test]
fn test_find_note_by_source_skips_dotfiles() {
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = dir.path();
    let hidden = vault.join(".obsidian");
    std::fs::create_dir_all(&hidden).expect("mkdir");

    let note_content = "---\nsource: \"https://example.com/hidden\"\n---\n";
    std::fs::write(hidden.join("config.md"), note_content).expect("write");

    let found = find_note_by_source(vault, "https://example.com/hidden");
    assert!(found.is_none(), "should skip dot-prefixed directories");
}

// ---------------------------------------------------------------------------
// `publish_url_note`: the plan -> render -> size gate -> store -> publish order
// (docs/design/2026-10-22-capture-all-diagrams.md, Store and publish ordering).
// These run through the note writer itself, not `process_x_post_from`.
// ---------------------------------------------------------------------------

use crate::pipeline::figures::PlannedFigure;
use crate::slides::cleanup::Owned;

const DIR: &str = "system/attachments/images/2026-10";

fn rel(name: &str) -> String {
    format!("{DIR}/{name}")
}

fn planned(name: &str, bytes: &[u8]) -> PlannedFigure {
    PlannedFigure {
        vault_path: rel(name),
        basename: name.to_string(),
        bytes: bytes.to_vec(),
    }
}

/// A vault with `inbox/` and an old note at `inbox/note.md` owning `old_images`
/// (each file written), plus a writable archive root.
struct Fixture {
    vault: tempfile::TempDir,
    archive_root: tempfile::TempDir,
    distilled: vault::distilled::Distilled,
}

impl Fixture {
    fn new(old_images: &[&str]) -> Self {
        let vault = tempfile::TempDir::new().unwrap();
        let inbox = vault.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::create_dir_all(vault.path().join(DIR)).unwrap();
        if !old_images.is_empty() {
            let list: String = old_images.iter().map(|n| format!("  - {}\n", rel(n))).collect();
            std::fs::write(
                inbox.join("note.md"),
                format!("---\ntitle: old\nimages:\n{list}---\nOLD BODY\n"),
            )
            .unwrap();
            for name in old_images {
                std::fs::write(vault.path().join(rel(name)), format!("old {name}")).unwrap();
            }
        }
        Self {
            vault,
            archive_root: tempfile::TempDir::new().unwrap(),
            distilled: vault::distilled::Distilled {
                summary: "s".to_string(),
                ..Default::default()
            },
        }
    }

    fn inbox(&self) -> PathBuf {
        self.vault.path().join("inbox")
    }

    fn old_note(&self) -> PathBuf {
        self.inbox().join("note.md")
    }

    fn old_owned(&self) -> Owned {
        crate::slides::cleanup::read_old_owned(&self.old_note()).unwrap()
    }

    fn publish(
        &self,
        archive_root: &Path,
        note: &[u8],
        figures: &[PlannedFigure],
        degraded: bool,
        max_note_bytes: usize,
    ) -> std::result::Result<(PathBuf, Vec<String>), UrlPublishError> {
        let archive = crate::archive::SourceArchive {
            root: archive_root,
            trace_id: "tr-new",
            distilled: &self.distilled,
            source_md: None,
        };
        let old_owned = self.old_owned();
        let old_note = self.old_note();
        let inbox = self.inbox();
        publish_url_note(&UrlPublish {
            archive: &archive,
            desired_path: &old_note,
            note_bytes: note,
            replaces: old_note.exists().then_some(old_note.as_path()),
            vault_dirs: std::slice::from_ref(&inbox),
            vault_root: self.vault.path(),
            max_note_bytes,
            figures,
            image_stage_degraded: degraded,
            old_owned: &old_owned,
            new_slides: &[],
        })
    }
}

const NOTE_CEILING: usize = 64 * 1024;

#[test]
fn reingest_that_drops_one_of_two_images_keeps_one_and_removes_the_other() {
    let fx = Fixture::new(&["tr-old-1-aaaaaaaa.png", "tr-old-2-bbbbbbbb.png"]);
    // The new note keeps the first image's path and drops the second.
    let keep = planned("tr-old-1-aaaaaaaa.png", b"new bytes ignored: path reused");
    let (path, orphans) = fx
        .publish(
            fx.archive_root.path(),
            b"---\ntitle: new\n---\nNEW\n",
            &[keep],
            false,
            NOTE_CEILING,
        )
        .unwrap();
    assert_eq!(path, fx.old_note());
    assert_eq!(orphans, vec![rel("tr-old-2-bbbbbbbb.png")]);
    assert!(
        fx.vault.path().join(rel("tr-old-1-aaaaaaaa.png")).exists(),
        "kept file stays"
    );
    assert!(
        !fx.vault.path().join(rel("tr-old-2-bbbbbbbb.png")).exists(),
        "dropped file is removed"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "---\ntitle: new\n---\nNEW\n");
}

#[test]
fn note_over_the_size_gate_writes_no_asset_and_no_note() {
    let fx = Fixture::new(&[]);
    // No old note, so nothing under `system/attachments` exists but its dir.
    std::fs::remove_dir_all(fx.vault.path().join("system")).unwrap();
    let oversize = vec![b'x'; 200];
    let err = fx
        .publish(
            fx.archive_root.path(),
            &oversize,
            &[planned("tr-new-1-cccccccc.png", b"png")],
            false,
            100,
        )
        .unwrap_err();
    assert!(matches!(err, UrlPublishError::SizeGate(_)), "got {err:?}");
    assert_eq!(err.failure_stage(), FailureStage::QualityBlocked);
    assert!(
        !fx.vault.path().join("system").exists(),
        "the gate runs before any asset is written"
    );
    assert!(!fx.old_note().exists());
    assert_eq!(std::fs::read_dir(fx.inbox()).unwrap().count(), 0);
    assert!(
        !crate::archive::is_durable(fx.archive_root.path(), "tr-new"),
        "the gate runs before the archive too"
    );
}

#[test]
fn publish_failure_removes_the_created_file_and_leaves_a_reused_path() {
    let fx = Fixture::new(&[]);
    let reused = fx.vault.path().join(rel("tr-new-1-aaaaaaaa.png"));
    std::fs::write(&reused, b"pre-existing").unwrap();
    let created = fx.vault.path().join(rel("tr-new-2-bbbbbbbb.png"));
    // A file where the archive root dir should be makes the archive write fail
    // after `store` has already run.
    let bad_root_parent = tempfile::TempDir::new().unwrap();
    let bad_root = bad_root_parent.path().join("sources");
    std::fs::write(&bad_root, b"not a dir").unwrap();

    let err = fx
        .publish(
            &bad_root,
            b"---\ntitle: new\n---\n",
            &[
                planned("tr-new-1-aaaaaaaa.png", b"would clobber"),
                planned("tr-new-2-bbbbbbbb.png", b"fresh"),
            ],
            false,
            NOTE_CEILING,
        )
        .unwrap_err();
    assert!(
        matches!(err, UrlPublishError::Publish(PublishError::ArchiveWrite(_))),
        "got {err:?}"
    );
    assert_eq!(err.failure_stage(), FailureStage::PublishFailed);
    assert!(!created.exists(), "the file this run created is removed");
    assert_eq!(
        std::fs::read(&reused).unwrap(),
        b"pre-existing",
        "a reused path is never touched"
    );
    assert!(!fx.old_note().exists(), "no note on a failed publish");
}

#[test]
fn degraded_reingest_over_a_note_that_owns_images_fails_fetch_failed_and_changes_nothing() {
    let fx = Fixture::new(&["tr-old-1-aaaaaaaa.png"]);
    let old_bytes = std::fs::read(fx.old_note()).unwrap();
    let err = fx
        .publish(
            fx.archive_root.path(),
            b"---\ntitle: worse\n---\nWORSE\n",
            &[planned("tr-new-1-dddddddd.png", b"new")],
            true,
            NOTE_CEILING,
        )
        .unwrap_err();
    assert!(
        matches!(err, UrlPublishError::DegradedReingest { owned: 1 }),
        "got {err:?}"
    );
    assert_eq!(err.failure_stage(), FailureStage::FetchFailed);
    assert_eq!(
        std::fs::read(fx.old_note()).unwrap(),
        old_bytes,
        "old note bytes unchanged"
    );
    assert!(
        fx.vault.path().join(rel("tr-old-1-aaaaaaaa.png")).exists(),
        "old file unchanged"
    );
    assert!(
        !fx.vault.path().join(rel("tr-new-1-dddddddd.png")).exists(),
        "no new file written"
    );
    assert!(!crate::archive::is_durable(fx.archive_root.path(), "tr-new"));
}

#[test]
fn degraded_fresh_ingest_publishes_as_usual() {
    let fx = Fixture::new(&[]);
    let (path, orphans) = fx
        .publish(
            fx.archive_root.path(),
            b"---\ntitle: new\n---\n",
            &[planned("tr-new-1-eeeeeeee.png", b"img")],
            true,
            NOTE_CEILING,
        )
        .unwrap();
    assert!(path.is_file());
    assert!(orphans.is_empty());
    assert!(fx.vault.path().join(rel("tr-new-1-eeeeeeee.png")).exists());
}

#[test]
fn degraded_reingest_over_a_note_with_no_images_publishes() {
    // Owns `slides:` only: the fail-closed rule is about images.
    let fx = Fixture::new(&[]);
    std::fs::write(fx.old_note(), "---\ntitle: old\nslides:\n  - s/a.jpg\n---\nOLD\n").unwrap();
    let (path, _) = fx
        .publish(
            fx.archive_root.path(),
            b"---\ntitle: new\n---\n",
            &[],
            true,
            NOTE_CEILING,
        )
        .unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "---\ntitle: new\n---\n");
}

#[test]
fn images_disabled_reingest_writes_empty_images_and_removes_the_old_files() {
    let fx = Fixture::new(&["tr-old-1-aaaaaaaa.png", "tr-old-2-bbbbbbbb.png"]);
    let (_, orphans) = fx
        .publish(
            fx.archive_root.path(),
            b"---\ntitle: new\n---\n",
            &[],
            false,
            NOTE_CEILING,
        )
        .unwrap();
    assert_eq!(orphans.len(), 2);
    assert!(!fx.vault.path().join(rel("tr-old-1-aaaaaaaa.png")).exists());
    assert!(!fx.vault.path().join(rel("tr-old-2-bbbbbbbb.png")).exists());
}
