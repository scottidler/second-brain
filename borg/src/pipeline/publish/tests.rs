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
