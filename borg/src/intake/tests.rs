#![allow(clippy::unwrap_used)]

use super::*;

/// Capture writes BOTH the sidecar (verbatim bytes) and a `received` receipts
/// row. This is the immediate-capture invariant.
#[test]
fn capture_writes_sidecar_and_received_row() {
    let conn = receipts::open_memory().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let url = "https://example.com/article";

    record_received_with_sidecar_to(
        &conn,
        root,
        IngestMethod::Http,
        IntakeKind::Url,
        url,
        url.as_bytes(),
        "tr-cap",
    )
    .unwrap();

    // Sidecar holds the verbatim input.
    let sidecar = std::fs::read(intake::raw_input_path(root, "tr-cap")).unwrap();
    assert_eq!(sidecar, url.as_bytes());

    // Receipts row exists in `received` with the mapped kind + preview.
    let r = receipts::get(&conn, "tr-cap").unwrap().expect("receipts row");
    assert_eq!(r.status, "received");
    assert_eq!(r.method, "http");
    assert_eq!(r.kind, "url");
    assert_eq!(r.raw_input, url);
}

/// A sidecar-write failure propagates as Err - the door must report Failed,
/// never silently drop the input. (Vault root is a regular file, so the
/// sidecar directory cannot be created.)
#[test]
fn capture_propagates_sidecar_write_failure() {
    let conn = receipts::open_memory().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let not_a_dir = tmp.path().join("iam-a-file");
    std::fs::write(&not_a_dir, b"x").unwrap();

    let err = record_received_with_sidecar_to(
        &conn,
        &not_a_dir,
        IngestMethod::Http,
        IntakeKind::Url,
        "https://x.com",
        b"https://x.com",
        "tr-fail",
    );
    assert!(err.is_err(), "sidecar write failure must propagate");
}

/// Failure helper marks an existing `received` row failed with the GIVEN
/// stage, and does NOT clobber the row's real captured kind/raw_input.
#[test]
fn failure_marks_existing_row_without_clobbering() {
    let conn = receipts::open_memory().unwrap();
    // Prior capture: a real photo with its real kind + descriptor.
    receipts::record_received(
        &conn,
        "tr-x",
        IngestMethod::Telegram.into(),
        ReceiptKind::Binary,
        "[photo: a.jpg]",
    )
    .unwrap();

    record_failure_at_door_to(
        &conn,
        IngestMethod::Telegram,
        "tr-x",
        FailureStage::IntakeRejected,
        "chat not allowed",
    )
    .unwrap();

    let r = receipts::get(&conn, "tr-x").unwrap().expect("row");
    assert_eq!(r.status, "failed");
    assert_eq!(r.failure_stage.as_deref(), Some("intake-rejected"));
    assert_eq!(r.failure_reason.as_deref(), Some("chat not allowed"));
    // INSERT OR IGNORE preserved the original capture, not the cold-path values.
    assert_eq!(r.kind, "binary", "real kind preserved");
    assert_eq!(r.raw_input, "[photo: a.jpg]", "real raw_input preserved");
}

/// The gap the advisor flagged: a failure with NO prior `received` row still
/// lands a `failed` row (the upsert path), instead of silently no-opping.
#[test]
fn failure_with_no_prior_row_still_lands_failed_row() {
    let conn = receipts::open_memory().unwrap();
    assert!(
        receipts::get(&conn, "tr-cold").unwrap().is_none(),
        "precondition: no row yet"
    );

    record_failure_at_door_to(
        &conn,
        IngestMethod::Signal,
        "tr-cold",
        FailureStage::IntakeRejected,
        "cold reject",
    )
    .unwrap();

    let r = receipts::get(&conn, "tr-cold")
        .unwrap()
        .expect("upsert created the row");
    assert_eq!(r.status, "failed");
    assert_eq!(r.failure_stage.as_deref(), Some("intake-rejected"));
    assert_eq!(r.raw_input, "cold reject", "cold-path raw_input is the reason");
}

/// Per-site stage is written faithfully - FetchFailed is not collapsed to
/// IntakeRejected (the Architect-flagged regression).
#[test]
fn failure_preserves_fetch_failed_stage() {
    let conn = receipts::open_memory().unwrap();
    receipts::record_received(
        &conn,
        "tr-sig",
        IngestMethod::Signal.into(),
        ReceiptKind::Binary,
        "[fetch]",
    )
    .unwrap();

    record_failure_at_door_to(
        &conn,
        IngestMethod::Signal,
        "tr-sig",
        FailureStage::FetchFailed,
        "failed to materialise Signal payload",
    )
    .unwrap();

    let r = receipts::get(&conn, "tr-sig").unwrap().expect("row");
    assert_eq!(
        r.failure_stage.as_deref(),
        Some("fetch-failed"),
        "FetchFailed must survive"
    );
}
